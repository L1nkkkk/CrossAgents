// Runs sandboxed, with context isolation. Exposes only the calls of
// `CrossAgentsBridge`; no Electron or Node API reaches the page.

import { contextBridge, ipcRenderer, type IpcRendererEvent } from "electron";
import type { ConnectionState, CrossAgentsBridge, HubResponse, PairStep, UpdateState } from "../src/bridge";

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

const bridge: CrossAgentsBridge = {
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
    const error: string | null = await ipcRenderer.invoke("shell:open-path", scope, path);
    if (error) throw new Error(error);
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
};

contextBridge.exposeInMainWorld("crossagents", bridge);
