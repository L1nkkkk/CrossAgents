import { join } from "node:path";

const PREFIX = "copy-";
const DAY = 24 * 60 * 60 * 1000;
export const copyRoot = (temp: string) => join(temp, "crossagents-open");
export const copyPrefix = (root: string) => join(root, PREFIX);
export function copyName(path: string): string {
  const name = path.split(/[\\/]/).pop()!.replace(/[<>:"/\\|?*\x00-\x1f]/g, "_").replace(/[. ]+$/g, "");
  if (!name || /^\.+$/.test(name)) return "file";
  return /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\.|$)/i.test(name) ? `_${name}` : name;
}
export function staleCopies(entries: readonly { name: string; modified: number }[], now: number): string[] {
  return entries.filter((entry) => /^copy-[a-zA-Z0-9]{6}$/.test(entry.name) && entry.modified < now - DAY).map((entry) => entry.name);
}
