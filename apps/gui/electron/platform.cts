import type { MenuItemConstructorOptions } from "electron";
import { join } from "node:path";

/** The native application menu on macOS, or no menu elsewhere. */
export function applicationMenu(platform: NodeJS.Platform): MenuItemConstructorOptions[] | null {
  if (platform !== "darwin") return null;
  return [
    { role: "appMenu", submenu: [{ role: "about" }, { type: "separator" }, { role: "hide" }, { role: "hideOthers" }, { role: "unhide" }, { type: "separator" }, { role: "quit" }] },
    { role: "editMenu", submenu: [{ role: "undo" }, { role: "redo" }, { type: "separator" }, { role: "cut" }, { role: "copy" }, { role: "paste" }, { role: "selectAll" }] },
    { role: "windowMenu", submenu: [{ role: "minimize" }, { role: "zoom" }, { role: "front" }] },
  ];
}

type Input = { type: string; key: string; code: string; control: boolean; meta: boolean; alt: boolean; shift: boolean };
/** Map a key event to a main-process command for the given platform. */
export function shortcut(platform: NodeJS.Platform, input: Input, development: boolean): "zoomIn" | "zoomOut" | "zoomReset" | "devtools" | "reload" | undefined {
  if (input.type !== "keyDown") return;
  const mod = platform === "darwin" ? input.meta : input.control;
  if (mod && !input.alt) {
    const zoom: Record<string, "zoomIn" | "zoomOut" | "zoomReset"> = { "=": "zoomIn", "+": "zoomIn", "-": "zoomOut", "0": "zoomReset" };
    if (zoom[input.key]) return zoom[input.key];
  }
  if (!development) return;
  const key = platform === "darwin" ? input.code : input.key.toLowerCase();
  if (input.key === "F12" || (mod && (platform === "darwin" ? input.alt : input.shift) && key === (platform === "darwin" ? "KeyI" : "i"))) return "devtools";
  if (mod && !input.shift && key === (platform === "darwin" ? "KeyR" : "r")) return "reload";
}

/** The tray resource filename for the platform. */
export function trayIcon(platform: NodeJS.Platform): string { return platform === "darwin" ? "iconTemplate.png" : "icon.ico"; }
/** Development connects to an independently run hub; packaged builds need the shipped executable. */
export function bundledHub(packaged: boolean, platform: NodeJS.Platform, resources: string, exists: (path: string) => boolean): boolean {
  return !packaged || exists(join(resources, "bin", platform === "win32" ? "crossagents.exe" : "crossagents"));
}
/** Ad-hoc signed macOS builds cannot use electron-updater. */
export function automaticUpdates(platform: NodeJS.Platform): boolean { return platform !== "darwin"; }
/** Text explaining where the window went when it was hidden. */
export function trayHint(platform: NodeJS.Platform, remote: boolean): { title: string; content: string } {
  const location = platform === "darwin" ? "菜单栏" : "任务栏右下角";
  return {
    title: remote ? "CrossAgents 仍在运行" : "CrossAgents 仍在后台运行",
    content: `点${location}的图标可以重新打开；右键图标可以${remote ? "退出" : "停止后台"}。`,
  };
}
