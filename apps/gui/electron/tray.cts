// The tray icon of a packaged GUI. The hub it starts runs on after the window closes, so the
// window closes to the tray instead of quitting: the icon shows the hub is there, and its menu
// opens the window again or stops the hub and its agents for good.

import { existsSync } from "node:fs";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { app, BrowserWindow, dialog, Menu, nativeImage, Tray } from "electron";
import type { LocalHub } from "./localHub.cjs";

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
export function createTray(icon: string, hub: LocalHub, show: () => void, log: (line: string) => void): AppTray {
  const tray = new Tray(nativeImage.createFromPath(icon));
  tray.setToolTip("CrossAgents");
  let stopping = false;

  const menu = (running: boolean) =>
    Menu.buildFromTemplate([
      { label: stopping ? "正在停止后台…" : running ? "后台运行中" : "后台未运行", enabled: false },
      { type: "separator" },
      { label: "打开 CrossAgents", click: show, enabled: !stopping },
      { type: "separator" },
      { label: "退出界面（后台继续运行）", click: () => app.quit(), enabled: !stopping },
      { label: "停止后台并退出", click: () => void stop(), enabled: !stopping },
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
    if (response !== 0 || stopping) return;
    stopping = true;
    tray.setToolTip("CrossAgents：正在停止后台…");
    // No window left to reconnect to the hub and start it again while it stops.
    for (const win of BrowserWindow.getAllWindows()) win.destroy();
    try {
      await hub.stopAll();
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
    tray.popUpContextMenu(menu(await hub.running()));
  });

  return {
    keep(win) {
      win.on("close", (event) => {
        if (quitting) return;
        event.preventDefault();
        win.hide();
        void hintOnce(tray);
      });
    },
  };
}

/** The first time the window goes to the tray, says where it went (once per user). */
async function hintOnce(tray: Tray) {
  const marker = join(app.getPath("userData"), "tray-hinted");
  if (existsSync(marker)) return;
  tray.displayBalloon({
    title: "CrossAgents 仍在后台运行",
    content: "点任务栏右下角的图标可以重新打开；右键图标可以停止后台。",
  });
  await mkdir(app.getPath("userData"), { recursive: true }).catch(() => {});
  await writeFile(marker, "").catch(() => {});
}
