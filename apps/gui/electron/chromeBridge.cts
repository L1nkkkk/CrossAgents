// Bridge between CrossAgents and the person's running Google Chrome / Edge browser via a
// lightweight unpacked Manifest V3 extension (Codex-style dual-backend architecture).
//
// While the built-in `<webview>` handles isolated previews and localhost pages, the Chrome
// extension lets agents (and the person) inspect and drive tabs in the real Chrome browser
// (`chrome:<tabId>`), inheriting all existing cookies, extensions, and Google Account sync
// without touching Chrome's encrypted SQLite files on disk.

import { execFile } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { join } from "node:path";
import { axSnapshot, backendNodeFor, consoleEntry, keyDefinition, quadCenter, type AxNode, type ConsoleEntry } from "./browserCdp.cjs";

export type CdpCookie = {
  name: string;
  value: string;
  domain: string;
  path?: string;
  secure?: boolean;
  httpOnly?: boolean;
  sameSite?: string;
  expires?: number;
  session?: boolean;
};

/** Converts a decrypted CDP cookie from Chrome into an Electron `CookiesSetDetails` payload. */
export function toElectronCookie(c: CdpCookie): {
  url: string;
  name: string;
  value: string;
  domain?: string;
  path: string;
  secure: boolean;
  httpOnly: boolean;
  sameSite: "unspecified" | "no_restriction" | "lax" | "strict";
  expirationDate?: number;
} | null {
  if (!c || typeof c.name !== "string" || !c.name || typeof c.value !== "string" || typeof c.domain !== "string" || !c.domain) {
    return null;
  }
  const cleanHost = c.domain.replace(/^\.+/, "");
  if (!cleanHost) return null;
  const hostPrefix = c.name.startsWith("__Host-");
  const securePrefix = hostPrefix || c.name.startsWith("__Secure-");
  const secure = securePrefix ? true : c.secure === true || c.sameSite === "None";
  const path = hostPrefix ? "/" : c.path || "/";
  const url = `${secure ? "https" : "http"}://${cleanHost}${path.startsWith("/") ? path : `/${path}`}`;
  const sameSite =
    c.sameSite === "Strict"
      ? "strict"
      : c.sameSite === "Lax"
        ? "lax"
        : c.sameSite === "None"
          ? "no_restriction"
          : "unspecified";
  const out: {
    url: string;
    name: string;
    value: string;
    domain?: string;
    path: string;
    secure: boolean;
    httpOnly: boolean;
    sameSite: "unspecified" | "no_restriction" | "lax" | "strict";
    expirationDate?: number;
  } = {
    url,
    name: c.name,
    value: c.value,
    path,
    secure,
    httpOnly: c.httpOnly === true,
    sameSite,
  };
  if (!hostPrefix && c.domain.startsWith(".")) {
    out.domain = c.domain;
  }
  if (!c.session && typeof c.expires === "number" && Number.isFinite(c.expires) && c.expires > 0) {
    out.expirationDate = c.expires;
  }
  return out;
}

/**
 * On Windows, a background process (`chrome.exe`) is blocked by `LockSetForegroundWindow` from
 * raising its window over the active Electron window. Calling `AppActivate` from the foreground
 * process group hands focus to Chrome so clicking「跳到外部 Chrome」visibly brings Chrome up.
 */
function focusChromeWindowOnHost(): void {
  if (process.platform === "win32") {
    execFile(
      "powershell.exe",
      ["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", "(New-Object -ComObject WScript.Shell).AppActivate('Chrome') | Out-Null"],
      { timeout: 3000, windowsHide: true },
      () => {},
    );
  } else if (process.platform === "darwin") {
    execFile("osascript", ["-e", 'tell application "Google Chrome" to activate'], { timeout: 3000 }, () => {});
  }
}

export const CHROME_BRIDGE_PORT = 7421;
const STALE_AFTER_MS = 15_000;
const COMMAND_TIMEOUT_MS = 18_000;
const LONG_POLL_MS = 12_000;
const CONSOLE_LIMIT = 200;

const TEXT_INPUT_TYPES = ["text", "search", "url", "tel", "email", "password", "number"];

const FOCUS_AND_SET = `function (text) {
  const isText = this instanceof HTMLTextAreaElement || (this instanceof HTMLInputElement && ${JSON.stringify(TEXT_INPUT_TYPES)}.includes(this.type));
  if (isText) {
    if (this.disabled || this.readOnly) return "none";
    this.focus();
    const proto = this instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    const setValue = Object.getOwnPropertyDescriptor(proto, "value").set;
    const old = this.value;
    const hasCaret = this.selectionStart !== null && this.selectionEnd !== null;
    const s = hasCaret ? this.selectionStart : old.length;
    const e = hasCaret ? this.selectionEnd : old.length;
    setValue.call(this, old.slice(0, s) + text + old.slice(e));
    if (hasCaret) {
      try { this.setSelectionRange(s + text.length, s + text.length); } catch (_) {}
    }
    this.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: text }));
    this.dispatchEvent(new Event("change", { bubbles: true }));
    return "value";
  }
  if (this.isContentEditable) {
    this.focus();
    return "editable";
  }
  return "none";
}`;

const CONTAINS_TEXT = `function (text) {
  const value = typeof this.value === "string" ? this.value : (this.textContent ?? "");
  return value.includes(text);
}`;

export type ChromeTabInfo = {
  /** Formatted as `chrome:<rawId>` so `browser_*` tools can distinguish it from built-in tabs. */
  tab: string;
  rawId: number;
  url: string;
  title: string;
  active: boolean;
};

export type ChromeBridgeStatus = {
  connected: boolean;
  port: number;
  extensionDir: string;
  tabs: ChromeTabInfo[];
};

export type BridgeCommand =
  | { id: number; kind: "open"; url: string; tabId?: number }
  | { id: number; kind: "navigate"; tabId: number; action: "back" | "forward" | "reload" }
  | { id: number; kind: "activate"; tabId: number }
  | { id: number; kind: "cdp"; tabId: number; method: string; params?: Record<string, unknown> }
  | { id: number; kind: "screenshot"; tabId: number };

export type BridgeCommandInput = BridgeCommand extends infer T ? (T extends BridgeCommand ? Omit<T, "id"> : never) : never;

export type BridgeReply = {
  id: number;
  ok: boolean;
  value?: unknown;
  error?: string;
};

export function parseChromeTabId(tab: unknown): number | null {
  if (typeof tab !== "string") return null;
  const match = /^chrome:(\d+)$/.exec(tab.trim());
  if (!match) return null;
  const n = Number(match[1]);
  return Number.isInteger(n) && n > 0 ? n : null;
}

/**
 * Pure coordinator for the Chrome extension bridge: tracks reported Chrome tabs, dispatches
 * commands to the extension's poll loop, and implements the `browser_*` tool semantics on top
 * of CDP responses from Chrome.
 */
export class ChromeBridgeCore {
  private tabs: ChromeTabInfo[] = [];
  private lastSeen = 0;
  private seq = 1;
  private queue: BridgeCommand[] = [];
  private waiters = new Map<number, { resolve: (v: any) => void; reject: (e: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  private pollWaiter: ((cmds: BridgeCommand[]) => void) | null = null;
  private refsByTab = new Map<number, Map<number, number>>();
  private logsByTab = new Map<number, ConsoleEntry[]>();

  constructor(
    public readonly extensionDir: string,
    public port: number = CHROME_BRIDGE_PORT,
    private readonly timeoutMs: number = COMMAND_TIMEOUT_MS,
  ) {}

  isConnected(now = Date.now()): boolean {
    return this.lastSeen > 0 && now - this.lastSeen < STALE_AFTER_MS;
  }

  status(now = Date.now()): ChromeBridgeStatus {
    const connected = this.isConnected(now);
    return {
      connected,
      port: this.port,
      extensionDir: this.extensionDir,
      tabs: connected ? this.tabs.slice() : [],
    };
  }

  /** Called when the extension posts its current tabs and any console events to `/poll`. */
  noteHeartbeat(
    rawTabs: readonly { id?: unknown; url?: unknown; title?: unknown; active?: unknown }[],
    events?: readonly { tabId?: unknown; method?: unknown; params?: unknown }[],
    now = Date.now(),
  ): BridgeCommand[] {
    this.lastSeen = now;
    const next: ChromeTabInfo[] = [];
    for (const item of rawTabs) {
      if (!item || typeof item !== "object") continue;
      const rawId = Number(item.id);
      if (!Number.isInteger(rawId) || rawId <= 0) continue;
      const url = typeof item.url === "string" ? item.url : "";
      if (/^(chrome|edge|devtools|chrome-extension):/i.test(url)) continue;
      const title = typeof item.title === "string" && item.title.trim() ? item.title.trim() : url || "Chrome 标签页";
      next.push({
        tab: `chrome:${rawId}`,
        rawId,
        url,
        title,
        active: item.active === true,
      });
    }
    this.tabs = next;

    if (Array.isArray(events)) {
      for (const ev of events) {
        const tabId = Number(ev?.tabId);
        const method = typeof ev?.method === "string" ? ev.method : "";
        if (!Number.isInteger(tabId) || !method) continue;
        const entry = consoleEntry(method, ev.params);
        if (!entry) continue;
        const list = this.logsByTab.get(tabId) ?? [];
        list.push(entry);
        if (list.length > CONSOLE_LIMIT) list.splice(0, list.length - CONSOLE_LIMIT);
        this.logsByTab.set(tabId, list);
      }
    }

    if (this.queue.length > 0) {
      return this.queue.splice(0, this.queue.length);
    }
    return [];
  }

  /** Waits up to `waitMs` for a command if none is queued right now. */
  waitForCommands(waitMs = LONG_POLL_MS): Promise<BridgeCommand[]> {
    if (this.queue.length > 0) {
      return Promise.resolve(this.queue.splice(0, this.queue.length));
    }
    return new Promise((resolve) => {
      const prev = this.pollWaiter;
      if (prev) prev([]);
      const timer = setTimeout(() => {
        if (this.pollWaiter === wake) this.pollWaiter = null;
        resolve([]);
      }, waitMs);
      const wake = (cmds: BridgeCommand[]) => {
        clearTimeout(timer);
        if (this.pollWaiter === wake) this.pollWaiter = null;
        resolve(cmds);
      };
      this.pollWaiter = wake;
    });
  }

  /** Settles a command that the extension finished running. */
  noteReply(reply: BridgeReply, now = Date.now()): void {
    this.lastSeen = now;
    const pending = this.waiters.get(reply.id);
    if (!pending) return;
    clearTimeout(pending.timer);
    this.waiters.delete(reply.id);
    if (reply.ok) pending.resolve(reply.value);
    else pending.reject(new Error(reply.error || "Chrome 扩展执行失败"));
  }

  sendCommand<T = any>(cmd: BridgeCommandInput): Promise<T> {
    if (!this.isConnected()) {
      return Promise.reject(new Error("未连接到本机 Chrome 扩展：请先在浏览器栏点「Chrome」按钮加载扩展"));
    }
    const id = this.seq++;
    const full = { ...cmd, id } as BridgeCommand;
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiters.delete(id);
        reject(new Error("等待本机 Chrome 响应超时"));
      }, this.timeoutMs);
      this.waiters.set(id, { resolve, reject, timer });
      if (this.pollWaiter) {
        const wake = this.pollWaiter;
        this.pollWaiter = null;
        wake([full]);
      } else {
        this.queue.push(full);
      }
    });
  }

  private cdp<T = any>(tabId: number, method: string, params?: Record<string, unknown>): Promise<T> {
    return this.sendCommand<T>({ kind: "cdp", tabId, method, params });
  }

  private mustTabId(tab: unknown): number {
    const id = parseChromeTabId(tab);
    if (!id) throw new Error(`无效的 Chrome 标签页标识：${String(tab)}（格式应为 chrome:<id>）`);
    return id;
  }

  async activate(tab: string): Promise<{ ok: true }> {
    const tabId = this.mustTabId(tab);
    await this.sendCommand({ kind: "activate", tabId });
    focusChromeWindowOnHost();
    return { ok: true };
  }

  /**
   * Reads decrypted cookies from the person's running Chrome via the already-attached CDP
   * channel (`Storage.getCookies` or `Network.getCookies`). Works immediately without reloading
   * the unpacked extension because `cmd.kind === "cdp"` is already handled by `background.js`.
   */
  async fetchCookies(tab?: string, urls?: string[]): Promise<CdpCookie[]> {
    const targetId =
      (tab ? parseChromeTabId(tab) : null) ??
      this.tabs.find((t) => t.active)?.rawId ??
      this.tabs[0]?.rawId;
    if (!targetId) throw new Error("Chrome 中没有可读取 Cookie 的打开标签页");
    const wantedUrls = Array.isArray(urls) && urls.length > 0 ? urls : this.tabs.map((t) => t.url).filter(Boolean);
    try {
      const all = await this.cdp<{ cookies?: CdpCookie[] }>(targetId, "Storage.getCookies");
      if (Array.isArray(all?.cookies) && all.cookies.length > 0) return all.cookies;
    } catch {
      // Fall back to Network.getCookies on the page target.
    }
    await this.cdp(targetId, "Network.enable").catch(() => undefined);
    const res = await this.cdp<{ cookies?: CdpCookie[] }>(
      targetId,
      "Network.getCookies",
      wantedUrls.length > 0 ? { urls: wantedUrls } : undefined,
    );
    return Array.isArray(res?.cookies) ? res.cookies : [];
  }

  async runAction(input: Record<string, unknown>): Promise<unknown> {
    const tool = typeof input.tool === "string" ? input.tool : "";
    if (tool === "browser_open") {
      const url = typeof input.url === "string" ? input.url.trim() : "";
      if (!url) throw new Error("browser_open 要给 url");
      const targetTab = typeof input.tab === "string" && input.tab !== "chrome" && input.tab !== "chrome:new" ? this.mustTabId(input.tab) : undefined;
      const opened = await this.sendCommand<{ id: number; url: string; title: string }>({
        kind: "open",
        url,
        tabId: targetTab,
      });
      return { tab: `chrome:${opened.id}`, url: opened.url, title: opened.title || opened.url };
    }

    const tabId = this.mustTabId(input.tab);
    if (tool === "browser_navigate") {
      const action = input.action;
      if (action !== "back" && action !== "forward" && action !== "reload") {
        throw new Error("browser_navigate 的 action 是 back、forward 或 reload");
      }
      await this.sendCommand({ kind: "navigate", tabId, action });
      return { ok: true, action };
    }

    if (tool === "browser_snapshot") {
      await this.cdp(tabId, "Accessibility.enable");
      await this.cdp(tabId, "DOM.enable");
      await this.cdp(tabId, "Runtime.enable");
      const { nodes } = await this.cdp<{ nodes: AxNode[] }>(tabId, "Accessibility.getFullAXTree");
      const snap = axSnapshot(nodes ?? []);
      this.refsByTab.set(tabId, snap.refs);
      const known = this.tabs.find((t) => t.rawId === tabId);
      return {
        text: snap.text,
        url: known?.url ?? "",
        title: known?.title ?? "Chrome",
      };
    }

    if (tool === "browser_click") {
      const refs = this.refsByTab.get(tabId) ?? new Map<number, number>();
      const backendNodeId = backendNodeFor(refs, Number(input.ref));
      await this.cdp(tabId, "DOM.scrollIntoViewIfNeeded", { backendNodeId }).catch(() => undefined);
      let box: { model: { content: number[] } };
      try {
        box = await this.cdp(tabId, "DOM.getBoxModel", { backendNodeId });
      } catch {
        throw new Error("这个元素已不在页面上或看不见，请重新 browser_snapshot");
      }
      const point = quadCenter(box.model.content);
      await this.cdp(tabId, "Input.dispatchMouseEvent", { type: "mousePressed", ...point, button: "left", clickCount: 1 });
      await this.cdp(tabId, "Input.dispatchMouseEvent", { type: "mouseReleased", ...point, button: "left", clickCount: 1 });
      return { ok: true };
    }

    if (tool === "browser_type") {
      const refs = this.refsByTab.get(tabId) ?? new Map<number, number>();
      const ref = Number(input.ref);
      const text = typeof input.text === "string" ? input.text : String(input.text ?? "");
      const backendNodeId = backendNodeFor(refs, ref);
      let resolved: { object: { objectId?: string } };
      try {
        resolved = await this.cdp(tabId, "DOM.resolveNode", { backendNodeId });
      } catch {
        throw new Error(`ref=${ref} 不能输入文字（不是输入框），请重新 browser_snapshot`);
      }
      const objectId = resolved.object?.objectId;
      if (!objectId) throw new Error(`ref=${ref} 不能输入文字（不是输入框），请重新 browser_snapshot`);
      try {
        const { result: kindRes } = await this.cdp<{ result: { value: string } }>(tabId, "Runtime.callFunctionOn", {
          objectId,
          functionDeclaration: FOCUS_AND_SET,
          arguments: [{ value: text }],
          returnByValue: true,
        });
        if (kindRes.value === "none") throw new Error(`ref=${ref} 不能输入文字（不是输入框），请重新 browser_snapshot`);
        if (kindRes.value === "editable") {
          await this.cdp(tabId, "Input.insertText", { text });
        }
        const { result: writtenRes } = await this.cdp<{ result: { value: boolean } }>(tabId, "Runtime.callFunctionOn", {
          objectId,
          functionDeclaration: CONTAINS_TEXT,
          arguments: [{ value: text }],
          returnByValue: true,
        });
        if (!writtenRes.value) throw new Error(`ref=${ref} 的文字没有写进输入框`);
      } finally {
        await this.cdp(tabId, "Runtime.releaseObject", { objectId }).catch(() => undefined);
      }
      if (input.submit === true) {
        const enter = keyDefinition("Enter")!;
        await this.cdp(tabId, "Input.dispatchKeyEvent", {
          type: "keyDown",
          key: enter.key,
          code: enter.code,
          windowsVirtualKeyCode: enter.keyCode,
          nativeVirtualKeyCode: enter.keyCode,
          text: enter.text,
          unmodifiedText: enter.text,
        });
        await this.cdp(tabId, "Input.dispatchKeyEvent", {
          type: "keyUp",
          key: enter.key,
          code: enter.code,
          windowsVirtualKeyCode: enter.keyCode,
          nativeVirtualKeyCode: enter.keyCode,
        });
      }
      return { ok: true };
    }

    if (tool === "browser_press") {
      const key = typeof input.key === "string" ? input.key : "";
      const def = keyDefinition(key);
      if (!def) throw new Error(`不认识的键名：${key}（用 CDP 键名，如 Enter、Tab、ArrowDown、Escape）`);
      const base = { key: def.key, code: def.code, windowsVirtualKeyCode: def.keyCode, nativeVirtualKeyCode: def.keyCode };
      const text = def.text === undefined ? {} : { text: def.text, unmodifiedText: def.text };
      await this.cdp(tabId, "Input.dispatchKeyEvent", { type: "keyDown", ...base, ...text });
      await this.cdp(tabId, "Input.dispatchKeyEvent", { type: "keyUp", ...base });
      return { ok: true };
    }

    if (tool === "browser_scroll") {
      const dy = Number(input.dy);
      if (!Number.isFinite(dy)) throw new Error("browser_scroll 的 dy 要是数字");
      const { result } = await this.cdp<{ result: { value: { w: number; h: number } } }>(tabId, "Runtime.evaluate", {
        expression: "({ w: innerWidth, h: innerHeight })",
        returnByValue: true,
      });
      const x = Math.round((result.value?.w || 800) / 2);
      const y = Math.round((result.value?.h || 600) / 2);
      await this.cdp(tabId, "Input.dispatchMouseEvent", { type: "mouseWheel", x, y, deltaX: 0, deltaY: dy });
      return { ok: true };
    }

    if (tool === "browser_screenshot") {
      const shot = await this.sendCommand<{ base64: string }>({ kind: "screenshot", tabId });
      return { image_base64: shot.base64, mime: "image/png" };
    }

    if (tool === "browser_console") {
      return (this.logsByTab.get(tabId) ?? []).slice();
    }

    throw new Error(`不认识的 Chrome 浏览器操作：${tool}`);
  }
}

/** Writes the unpacked Manifest V3 Chrome extension to `dir` so the person can load it from Chrome. */
export function ensureChromeExtensionFiles(dir: string, port = CHROME_BRIDGE_PORT): void {
  mkdirSync(dir, { recursive: true });
  const manifest = {
    manifest_version: 3,
    name: "CrossAgents Chrome Bridge",
    version: "1.0.0",
    description: "将本机 Chrome 的标签页、登录态与插件桥接给 CrossAgents 桌面端与 Agent 使用。",
    permissions: ["tabs", "debugger", "alarms"],
    host_permissions: [`http://127.0.0.1:${port}/*`, "http://127.0.0.1/*"],
    background: {
      service_worker: "background.js",
    },
    action: {
      default_title: "CrossAgents Chrome Bridge",
      default_popup: "popup.html",
    },
  };

  const backgroundJs = `// CrossAgents Chrome Bridge (MV3 Service Worker)
const BRIDGE = "http://127.0.0.1:${port}";
const attachedTabs = new Set();
let pendingEvents = [];
let polling = false;

chrome.debugger.onEvent.addListener((source, method, params) => {
  if (!source.tabId) return;
  if (method === "Runtime.consoleAPICalled" || method === "Runtime.exceptionThrown") {
    pendingEvents.push({ tabId: source.tabId, method, params });
    if (pendingEvents.length > 100) pendingEvents.splice(0, pendingEvents.length - 100);
  }
});

chrome.debugger.onDetach.addListener((source) => {
  if (source.tabId) attachedTabs.delete(source.tabId);
});

async function ensureAttached(tabId) {
  if (attachedTabs.has(tabId)) return;
  try {
    await chrome.debugger.attach({ tabId }, "1.3");
    attachedTabs.add(tabId);
  } catch (err) {
    if (!String(err && err.message || err).includes("Already attached")) throw err;
    attachedTabs.add(tabId);
  }
}

async function executeCommand(cmd) {
  if (cmd.kind === "open") {
    if (cmd.tabId) {
      const updated = await chrome.tabs.update(cmd.tabId, { url: cmd.url, active: true });
      return { id: updated.id, url: updated.url || cmd.url, title: updated.title || cmd.url };
    }
    const created = await chrome.tabs.create({ url: cmd.url, active: true });
    return { id: created.id, url: created.url || cmd.url, title: created.title || cmd.url };
  }
  if (cmd.kind === "activate") {
    const tab = await chrome.tabs.update(cmd.tabId, { active: true });
    if (tab && tab.windowId) await chrome.windows.update(tab.windowId, { focused: true });
    return { ok: true };
  }
  if (cmd.kind === "navigate") {
    if (cmd.action === "reload") await chrome.tabs.reload(cmd.tabId);
    else if (cmd.action === "back") await chrome.tabs.goBack(cmd.tabId);
    else if (cmd.action === "forward") await chrome.tabs.goForward(cmd.tabId);
    return { ok: true };
  }
  if (cmd.kind === "screenshot") {
    await ensureAttached(cmd.tabId);
    const res = await chrome.debugger.sendCommand({ tabId: cmd.tabId }, "Page.captureScreenshot", { format: "png" });
    return { base64: res.data };
  }
  if (cmd.kind === "cdp") {
    await ensureAttached(cmd.tabId);
    return await chrome.debugger.sendCommand({ tabId: cmd.tabId }, cmd.method, cmd.params || {});
  }
  throw new Error("Unknown command kind: " + cmd.kind);
}

async function runCommand(cmd) {
  let payload;
  try {
    const value = await executeCommand(cmd);
    payload = { id: cmd.id, ok: true, value };
  } catch (err) {
    payload = { id: cmd.id, ok: false, error: String(err && err.message || err) };
  }
  try {
    await fetch(BRIDGE + "/reply", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    });
  } catch (_) {}
}

async function pollLoop() {
  if (polling) return;
  polling = true;
  while (true) {
    try {
      const tabs = await chrome.tabs.query({});
      const events = pendingEvents.splice(0, pendingEvents.length);
      const res = await fetch(BRIDGE + "/poll", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          tabs: tabs.map((t) => ({ id: t.id, url: t.url, title: t.title, active: t.active })),
          events,
        }),
      });
      if (!res.ok) throw new Error("HTTP " + res.status);
      const data = await res.json();
      if (Array.isArray(data.commands)) {
        for (const cmd of data.commands) void runCommand(cmd);
      }
    } catch (_) {
      await new Promise((r) => setTimeout(r, 2500));
    }
  }
}

chrome.alarms.create("crossagents-keepalive", { periodInMinutes: 0.4 });
chrome.alarms.onAlarm.addListener(() => { void pollLoop(); });
chrome.tabs.onUpdated.addListener(() => { void pollLoop(); });
void pollLoop();
`;

  const popupHtml = `<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8" />
  <style>
    body { margin: 0; padding: 12px 14px; width: 240px; font: 13px/1.5 system-ui, sans-serif; color: #1f2328; background: #fff; }
    .row { display: flex; align-items: center; gap: 8px; font-weight: 600; }
    .dot { width: 8px; height: 8px; border-radius: 50%; background: #10a37f; }
    .sub { margin-top: 6px; font-size: 12px; color: #57606a; }
  </style>
</head>
<body>
  <div class="row"><span class="dot"></span><span>CrossAgents Chrome 桥接</span></div>
  <div class="sub">扩展已运行，自动与本机 CrossAgents (127.0.0.1:${port}) 同步标签页与登录态。</div>
</body>
</html>
`;

  writeFileSync(join(dir, "manifest.json"), JSON.stringify(manifest, null, 2));
  writeFileSync(join(dir, "background.js"), backgroundJs);
  writeFileSync(join(dir, "popup.html"), popupHtml);
}

function readJsonBody(req: IncomingMessage): Promise<any> {
  return new Promise((resolve, reject) => {
    let data = "";
    req.setEncoding("utf8");
    req.on("data", (chunk) => {
      data += chunk;
      if (data.length > 16 * 1024 * 1024) {
        req.destroy();
        reject(new Error("payload too large"));
      }
    });
    req.on("end", () => {
      try {
        resolve(data ? JSON.parse(data) : {});
      } catch (err) {
        reject(err);
      }
    });
    req.on("error", reject);
  });
}

function setCors(req: IncomingMessage, res: ServerResponse): boolean {
  const origin = req.headers.origin;
  if (origin && !origin.startsWith("chrome-extension://") && !origin.startsWith("http://127.0.0.1:")) {
    return false;
  }
  if (origin) res.setHeader("Access-Control-Allow-Origin", origin);
  res.setHeader("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
  res.setHeader("Access-Control-Allow-Headers", "Content-Type");
  return true;
}

/** Starts the loopback HTTP bridge server for the Chrome extension. */
export function startChromeBridgeServer(core: ChromeBridgeCore, log: (line: string) => void): Server {
  const server = createServer(async (req, res) => {
    if (!setCors(req, res)) {
      res.writeHead(403).end();
      return;
    }
    if (req.method === "OPTIONS") {
      res.writeHead(204).end();
      return;
    }
    try {
      if (req.method === "POST" && req.url === "/poll") {
        const body = await readJsonBody(req);
        const immediate = core.noteHeartbeat(Array.isArray(body?.tabs) ? body.tabs : [], Array.isArray(body?.events) ? body.events : []);
        const commands = immediate.length > 0 ? immediate : await core.waitForCommands();
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ commands }));
        return;
      }
      if (req.method === "POST" && req.url === "/reply") {
        const body = await readJsonBody(req);
        if (typeof body?.id === "number") {
          core.noteReply({ id: body.id, ok: body.ok === true, value: body.value, error: body.error });
        }
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ ok: true }));
        return;
      }
      if (req.method === "GET" && req.url === "/status") {
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify(core.status()));
        return;
      }
      res.writeHead(404).end();
    } catch (err) {
      log(`chrome bridge: ${err instanceof Error ? err.message : String(err)}`);
      if (!res.headersSent) res.writeHead(400).end();
    }
  });

  server.on("error", (err) => {
    log(`chrome bridge listen: ${err instanceof Error ? err.message : String(err)}`);
  });
  server.listen(core.port, "127.0.0.1");
  return server;
}
