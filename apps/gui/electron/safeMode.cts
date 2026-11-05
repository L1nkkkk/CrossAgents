// Startup decision and durable loading marker, independent of Electron.
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

export type SafeMode = { enabled: boolean; reason: string | null };

export function decideSafeMode(argv: readonly string[], unfinishedLoading: boolean): SafeMode {
  if (argv.includes("--safe-mode")) return { enabled: true, reason: "通过 --safe-mode 启动，已跳过所有扩展。" };
  if (unfinishedLoading) return { enabled: true, reason: "上次启动在加载扩展时未能就绪，已自动进入安全模式并跳过所有扩展。" };
  return { enabled: false, reason: null };
}

export function loadingMarker(file: string) {
  return {
    unfinished: () => existsSync(file),
    start() {
      mkdirSync(dirname(file), { recursive: true });
      writeFileSync(file, "loading\n");
    },
    ready() { rmSync(file, { force: true }); },
  };
}
