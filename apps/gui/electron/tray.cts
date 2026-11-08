// The tray icon of a packaged GUI. The hub it starts runs on after the window closes, so the
// window closes to the tray instead of quitting: the icon shows the hub is there, and its menu
// opens the window again or stops the hub and its agents for good, and checks for or installs
// an update of the GUI.

import { existsSync } from "node:fs";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { app, BrowserWindow, dialog, Menu, nativeImage, Tray, type MenuItemConstructorOptions } from "electron";
import type { LocalHub } from "./localHub.cjs";
import type { Updates } from "./updates.cjs";

/** Set once the GUI is really quitting, so closing the window no longer just hides it. */
let quitting = false;
app.on("before-quit", () => {
  quitting = true;
});

export type AppTray = {
  /** Hides `win` to the tray when it is closed, unless the GUI is quitting. */
  keep(win: BrowserWindow): void;
};

/**
 * Puts the icon in the tray. `icon` is an .ico or .png file; `show` brings the window back
 * (creating it if needed); `log` gets what the tray did to the hub.
 */
export function createTray(
  icon: string,
  /** The local hub being managed; undefined in remote mode. */
  hub: () => LocalHub | undefined,
  /** In remote mode, the target and whether the tunnel is up (electron/remoteController.cts). */
  remote: () => { target: string; connected: boolean } | undefined,
  show: () => void, log: (line: string) => void, updates: Updates, restartSafeMode: () => void): AppTray {
  const tray = new Tray(nativeImage.createFromPath(icon));
  tray.setToolTip("CrossAgents");
  let stopping = false;
  let announced: string | undefined;
  updates.onChange((state) => {
    if (state.state !== "ready") return;
    tray.setToolTip(`CrossAgents：${state.version} 已下载，退出时安装`);
    // Said once per version: the window may be hidden in the tray for days.
    if (announced === state.version) return;
    announced = state.version;
    tray.displayBalloon({
      title: `CrossAgents ${state.version} 已下载`,
      content: "退出时会自动安装；也可以在侧栏底部或托盘菜单里「重启更新」。后台的 hub 和 agent 不会中断。",
    });
  });
  // The balloons say where things are; clicking one opens the window.
  tray.on("balloon-click", show);

  const update = (): MenuItemConstructorOptions => {
    const state = updates.state();
    switch (state.state) {
      case "ready":
        return { label: `重启并更新到 ${state.version}`, click: () => updates.install(), enabled: !stopping };
      case "downloading":
        return { label: `正在下载 ${state.version}（${state.percent}%）`, enabled: false };
      case "checking":
        return { label: "正在检查更新…", enabled: false };
      default:
        return { label: `检查更新（当前 ${state.current}）`, click: () => void updates.check(), enabled: !stopping };
    }
  };

  /** The first line: the local hub, or the remote one. The remote hub is never stopped from here (§13.1). */
  const menu = (running: boolean, far: { target: string; connected: boolean } | undefined) =>
    Menu.buildFromTemplate([
      {
        label: far
          ? `远程：${far.target}（${far.connected ? "已连接" : "未连接"}）`
          : stopping ? "正在停止后台…" : running ? "后台运行中" : "后台未运行",
        enabled: false,
      },
      { type: "separator" },
      { label: "打开 CrossAgents", click: show, enabled: !stopping },
      update(),
      { label: "以安全模式重启", click: restartSafeMode, enabled: !stopping },
      { type: "separator" },
      ...(far
        ? [{ label: "退出界面", click: () => app.quit() }]
        : [
          { label: "退出界面（后台继续运行）", click: () => app.quit(), enabled: !stopping },
          { label: "停止后台并退出", click: () => void stop(), enabled: !stopping },
        ]),
    ]);

  const stop = async () => {
    const { response } = await dialog.showMessageBox({
      type: "warning",
      title: "CrossAgents",
      message: "停止后台并退出？",
      detail: "hub 和所有 agent 都会停止，正在进行的工作会中断。下次打开 CrossAgents 时后台会重新启动。",
      buttons: ["停止并退出", "取消"],
      defaultId: 1,
      cancelId: 1,
      noLink: true,
    });
    const local = hub();
    if (response !== 0 || stopping || !local) return;
    stopping = true;
    tray.setToolTip("CrossAgents：正在停止后台…");
    // No window left to reconnect to the hub and start it again while it stops.
    for (const win of BrowserWindow.getAllWindows()) win.destroy();
    try {
      await local.stopAll();
      log("tray: stopped the hub and its agents");
    } catch (err) {
      const reason = err instanceof Error ? err.message : String(err);
      log(`tray: cannot stop the hub: ${reason}`);
      dialog.showErrorBox("CrossAgents", `后台没能停止：${reason}`);
    }
    app.quit();
  };

  tray.on("click", show);
  // Built when opened, so the first line says whether the hub runs now.
  tray.on("right-click", async () => {
    const far = remote();
    const local = hub();
    tray.popUpContextMenu(menu(far || !local ? false : await local.running(), far));
  });

  return {
    keep(win) {
      win.on("close", (event) => {
        if (quitting) return;
        event.preventDefault();
        win.hide();
        void hintOnce(tray, remote() !== undefined);
      });
    },
  };
}

/** The first time the window goes to the tray, says where it went (once per user). */
async function hintOnce(tray: Tray, remoteMode: boolean) {
  const marker = join(app.getPath("userData"), "tray-hinted");
  if (existsSync(marker)) return;
  tray.displayBalloon(remoteMode
    ? { title: "CrossAgents 仍在运行", content: "点任务栏右下角的图标可以重新打开；右键图标可以退出。" }
    : { title: "CrossAgents 仍在后台运行", content: "点任务栏右下角的图标可以重新打开；右键图标可以停止后台。" });
  await mkdir(app.getPath("userData"), { recursive: true }).catch(() => {});
  await writeFile(marker, "").catch(() => {});
}
