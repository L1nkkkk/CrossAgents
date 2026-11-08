// Updates of a packaged GUI (package.json `build.publish`): checked at launch and every few
// hours after, since the tray keeps the GUI running for days; downloaded in the background and
// installed when the GUI quits, or right away when the person asks. The settings page and the
// tray show where it stands.

import { app, BrowserWindow } from "electron";
import { autoUpdater } from "electron-updater";
import type { UpdateState } from "../src/bridge";
import { automaticUpdates } from "./platform.cjs";

/** Long enough not to bother GitHub, short enough that a release reaches a GUI left open. */
const CHECK_EVERY_MS = 4 * 60 * 60 * 1000;

export type Updates = {
  state(): UpdateState;
  /** Checks now, unless a check or download is already going; resolves with the outcome. */
  check(): Promise<UpdateState>;
  /** Quits and installs a downloaded update; the next launch upgrades the local hub. */
  install(): void;
  /** Called on every change (the tray rebuilds its menu from it). */
  onChange(listener: (state: UpdateState) => void): void;
};

export function startUpdates(log: (line: string) => void): Updates {
  if (!automaticUpdates(process.platform)) {
    const manual: UpdateState = { state: "manual", current: app.getVersion(), reason: "Mac 版没有 Apple 签名，不能自动更新：要更新就重新打包，再替换「应用程序」里的 CrossAgents。" };
    return { state: () => manual, check: async () => manual, install() {}, onChange() {} };
  }
  const listeners: ((state: UpdateState) => void)[] = [];
  let state: UpdateState = { state: "idle", current: app.getVersion() };
  const set = (next: UpdateState) => {
    state = next;
    for (const win of BrowserWindow.getAllWindows()) win.webContents.send("update:state", state);
    for (const listener of listeners) listener(state);
  };
  const current = app.getVersion();
  const now = () => new Date().toISOString();

  const updaterLog = (m: unknown) => log(`updater: ${String(m)}`);
  autoUpdater.logger = { info: updaterLog, warn: updaterLog, error: updaterLog, debug: () => {} };
  autoUpdater.autoDownload = true;
  autoUpdater.autoInstallOnAppQuit = true;
  autoUpdater.on("checking-for-update", () => set({ state: "checking", current }));
  autoUpdater.on("update-not-available", () => set({ state: "latest", current, checked_at: now() }));
  autoUpdater.on("update-available", (info) => set({ state: "downloading", current, version: info.version, percent: 0 }));
  autoUpdater.on("download-progress", (p) => {
    if (state.state === "downloading") set({ ...state, percent: Math.floor(p.percent) });
  });
  autoUpdater.on("update-downloaded", (info) => set({ state: "ready", current, version: info.version }));
  autoUpdater.on("error", (err) => {
    // A download that failed after the check found something: say it was the download.
    const message = (err instanceof Error ? err.message : String(err)).split("\n")[0];
    if (state.state !== "ready") set({ state: "error", current, message, checked_at: now() });
  });

  const check = async (): Promise<UpdateState> => {
    if (state.state === "checking" || state.state === "downloading" || state.state === "ready") return state;
    try {
      await autoUpdater.checkForUpdates();
    } catch (err) {
      log(`update check failed: ${(err instanceof Error ? err.message : String(err)).split("\n")[0]}`);
    }
    return state;
  };

  void check();
  setInterval(() => void check(), CHECK_EVERY_MS).unref();

  return {
    state: () => state,
    check,
    install() {
      if (state.state !== "ready") return;
      log(`updater: installing ${state.version} now, asked from the GUI`);
      // Silent, and the GUI starts again after: it then upgrades the local hub itself.
      autoUpdater.quitAndInstall(true, true);
    },
    onChange(listener) {
      listeners.push(listener);
    },
  };
}
