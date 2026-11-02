// Runs sandboxed, with context isolation. Exposes only the calls of
// `CrossAgentsBridge`; no Electron or Node API reaches the page.

import { contextBridge, ipcRenderer, type IpcRendererEvent } from "electron";
import type { CrossAgentsBridge, HubResponse, UpdateState } from "../src/bridge";

type Reply = { ok: true; response: HubResponse } | { ok: false; error: string };

function subscribe(channel: string, listener: (value: string) => void): () => void {
  const wrapped = (_event: IpcRendererEvent, value: string) => listener(value);
  ipcRenderer.on(channel, wrapped);
  return () => {
    ipcRenderer.removeListener(channel, wrapped);
  };
}

const bridge: CrossAgentsBridge = {
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
  pickFolder: () => ipcRenderer.invoke("dialog:pick-folder"),
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
