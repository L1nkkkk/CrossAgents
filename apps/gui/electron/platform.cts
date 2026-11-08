import type { MenuItemConstructorOptions } from "electron";

export function applicationMenu(platform: NodeJS.Platform): MenuItemConstructorOptions[] | null {
  if (platform !== "darwin") return null;
  return [
    { role: "appMenu", submenu: [{ role: "about" }, { type: "separator" }, { role: "hide" }, { role: "hideOthers" }, { role: "unhide" }, { type: "separator" }, { role: "quit" }] },
    { role: "editMenu", submenu: [{ role: "undo" }, { role: "redo" }, { type: "separator" }, { role: "cut" }, { role: "copy" }, { role: "paste" }, { role: "selectAll" }] },
    { role: "windowMenu", submenu: [{ role: "minimize" }, { role: "zoom" }, { role: "front" }] },
  ];
}

type Input = { type: string; key: string; control: boolean; meta: boolean; alt: boolean; shift: boolean };
export function shortcut(platform: NodeJS.Platform, input: Input, development: boolean): "zoomIn" | "zoomOut" | "zoomReset" | "devtools" | "reload" | undefined {
  if (input.type !== "keyDown") return;
  const mod = platform === "darwin" ? input.meta : input.control;
  if (mod && !input.alt) {
    const zoom: Record<string, "zoomIn" | "zoomOut" | "zoomReset"> = { "=": "zoomIn", "+": "zoomIn", "-": "zoomOut", "0": "zoomReset" };
    if (zoom[input.key]) return zoom[input.key];
  }
  if (!development) return;
  const key = input.key.toLowerCase();
  if (input.key === "F12" || (mod && (platform === "darwin" ? input.alt : input.shift) && key === "i")) return "devtools";
  if (mod && !input.shift && key === "r") return "reload";
}

export function trayIcon(platform: NodeJS.Platform): string { return platform === "darwin" ? "iconTemplate.png" : "icon.ico"; }
export function trayHint(platform: NodeJS.Platform, remote: boolean): { title: string; content: string } {
  const location = platform === "darwin" ? "菜单栏" : "任务栏右下角";
  return {
    title: remote ? "CrossAgents 仍在运行" : "CrossAgents 仍在后台运行",
    content: `点${location}的图标可以重新打开；右键图标可以${remote ? "退出" : "停止后台"}。`,
  };
}
