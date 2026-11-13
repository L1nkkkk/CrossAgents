// Runs sandboxed, with context isolation. Exposes only the calls of
// `CrossAgentsBridge`; no Electron or Node API reaches the page.

import { contextBridge, ipcRenderer, type IpcRendererEvent } from "electron";
import type { BookmarkEntry, ChromeBridgeStatus, ConnectionState, CrossAgentsBridge, HubResponse, PairStep, TailscaleStatus, UpdateState } from "../src/bridge";

type Reply = { ok: true; response: HubResponse } | { ok: false; error: string };

/** The main process answers `{ ok, state }` or `{ ok: false, error }`, so the reason reaches the page as is. */
async function connectionCall(channel: string, ...args: unknown[]): Promise<ConnectionState> {
  const reply: { ok: true; state: ConnectionState } | { ok: false; error: string } = await ipcRenderer.invoke(channel, ...args);
  if (!reply.ok) throw new Error(reply.error);
  return reply.state;
}

function subscribe(channel: string, listener: (value: string) => void): () => void {
  const wrapped = (_event: IpcRendererEvent, value: string) => listener(value);
  ipcRenderer.on(channel, wrapped);
  return () => {
    ipcRenderer.removeListener(channel, wrapped);
  };
}

/** The main process answers `{ ok, value }` or `{ ok: false, error }` for the browser (electron/browser.cts). */
async function browserCall<T>(channel: string, ...args: unknown[]): Promise<T> {
  const reply: { ok: true; value: T } | { ok: false; error: string } = await ipcRenderer.invoke(channel, ...args);
  if (!reply.ok) throw new Error(reply.error);
  return reply.value;
}

/**
 * `window.crossagents.browser` (run note browser/contract §3). Every call names the tab by its
 * webContents id (`webview.getWebContentsId()`); the main process refuses any other.
 */
export type BrowserBridge = {
  snapshot(wcId: number): Promise<{ text: string; url: string; title: string }>;
  click(wcId: number, ref: number): Promise<{ ok: true }>;
  type(wcId: number, ref: number, text: string, submit?: boolean): Promise<{ ok: true }>;
  press(wcId: number, key: string): Promise<{ ok: true }>;
  scroll(wcId: number, dy: number): Promise<{ ok: true }>;
  screenshot(wcId: number): Promise<{ base64: string; mime: "image/png" }>;
  console(wcId: number): Promise<{ level: string; text: string; ts: number }[]>;
  fileUrl(absPath: string): Promise<string>;
  onOpenTab(listener: (url: string) => void): () => void;
  bookmarks(): Promise<{ source: string | null; entries: BookmarkEntry[] }>;
  openExternal(url: string): Promise<{ ok: true }>;
  chromeStatus(): Promise<ChromeBridgeStatus>;
  chromeAction(payload: Record<string, unknown>): Promise<unknown>;
  chromeActivate(tab: string): Promise<{ ok: true }>;
  openExtensionFolder(): Promise<{ dir: string }>;
};

const browser: BrowserBridge = {
  snapshot: (wcId) => browserCall("browser:snapshot", wcId),
  async click(wcId, ref) {
    await browserCall("browser:click", wcId, ref);
    return { ok: true };
  },
  async type(wcId, ref, text, submit) {
    await browserCall("browser:type", wcId, ref, text, submit === true);
    return { ok: true };
  },
  async press(wcId, key) {
    await browserCall("browser:press", wcId, key);
    return { ok: true };
  },
  async scroll(wcId, dy) {
    await browserCall("browser:scroll", wcId, dy);
    return { ok: true };
  },
  screenshot: (wcId) => browserCall("browser:screenshot", wcId),
  console: (wcId) => browserCall("browser:console", wcId),
  fileUrl: (absPath) => browserCall("browser:file-url", absPath),
  onOpenTab: (listener) => subscribe("browser:open-tab", listener),
  bookmarks: () => browserCall("browser:bookmarks"),
  openExternal: (url) => browserCall("browser:open-external", url),
  chromeStatus: () => browserCall("browser:chrome-status"),
  chromeAction: (payload) => browserCall("browser:chrome-action", payload),
  chromeActivate: (tab) => browserCall("browser:chrome-activate", tab),
  openExtensionFolder: () => browserCall("browser:chrome-open-ext-dir"),
};

const bridge: CrossAgentsBridge & { browser: BrowserBridge } = {
  browser,
  extensions: {
    safeMode: () => ipcRenderer.invoke("extensions:safe-mode"),
    loading: () => ipcRenderer.invoke("extensions:loading"),
    ready: () => ipcRenderer.invoke("extensions:ready"),
  },
  async request(method, path, body) {
    const reply: Reply = await ipcRenderer.invoke("hub:request", method, path, body);
    if (!reply.ok) throw new Error(reply.error);
    return reply.response;
  },
  startEvents: () => ipcRenderer.send("hub:events-start"),
  onEvent: (listener) => subscribe("hub:event", listener),
  onStatus: (listener) => subscribe("hub:status", listener),
  terminalStart: (id, connection) => ipcRenderer.send("terminal:start", id, connection),
  terminalStop: (id) => ipcRenderer.send("terminal:stop", id),
  onTerminal(listener) {
    const wrapped = (_event: IpcRendererEvent, id: string, data: string) => listener(id, data);
    ipcRenderer.on("terminal:frame", wrapped);
    return () => {
      ipcRenderer.removeListener("terminal:frame", wrapped);
    };
  },
  devHub: () => ipcRenderer.invoke("hub:dev"),
  async useShippedHub() {
    const error: string | null = await ipcRenderer.invoke("hub:use-shipped");
    if (error) throw new Error(error);
  },
  async pickFolder() {
    const reply: string | null | { error: string } = await ipcRenderer.invoke("dialog:pick-folder");
    if (reply && typeof reply === "object") throw new Error(reply.error);
    return reply;
  },
  connection: {
    get: () => connectionCall("connection:get"),
    pair: (target, deviceName) => connectionCall("connection:pair", target, deviceName),
    pairCode: (code, deviceName) => connectionCall("connection:pair-code", code, deviceName),
    use: (mode) => connectionCall("connection:use", mode),
    reconnect: () => connectionCall("connection:reconnect"),
    forget: () => connectionCall("connection:forget"),
    onState(listener) {
      const wrapped = (_event: IpcRendererEvent, state: ConnectionState) => listener(state);
      ipcRenderer.on("connection:state", wrapped);
      return () => {
        ipcRenderer.removeListener("connection:state", wrapped);
      };
    },
    onPairStep(listener) {
      const wrapped = (_event: IpcRendererEvent, step: PairStep) => listener(step);
      ipcRenderer.on("connection:pair-step", wrapped);
      return () => {
        ipcRenderer.removeListener("connection:pair-step", wrapped);
      };
    },
  },
  async openPath(scope, path) {
    const result: { copy: boolean } | { error: string } = await ipcRenderer.invoke("shell:open-path", scope, path);
    if ("error" in result) throw new Error(result.error);
    return result;
  },
  updates: {
    get: () => ipcRenderer.invoke("update:get"),
    check: () => ipcRenderer.invoke("update:check"),
    install: () => ipcRenderer.send("update:install"),
    onState(listener) {
      const wrapped = (_event: IpcRendererEvent, state: UpdateState) => listener(state);
      ipcRenderer.on("update:state", wrapped);
      return () => {
        ipcRenderer.removeListener("update:state", wrapped);
      };
    },
  },
  setTheme: (theme) => ipcRenderer.send("theme:changed", theme),
  command: (name) => ipcRenderer.send("window:command", name),
  tailscale: {
    status: (): Promise<TailscaleStatus> => ipcRenderer.invoke("tailscale:status"),
  },
};

contextBridge.exposeInMainWorld("crossagents", bridge);
