import { join } from "node:path";

const PREFIX = "copy-";
const DAY = 24 * 60 * 60 * 1000;
/** Root for downloaded copies. */
export const copyRoot = (temp: string) => join(temp, "crossagents-open");
/** Prefix passed to mkdtemp for a private copy directory. */
export const copyPrefix = (root: string) => join(root, PREFIX);
/** Make a portable filename without allowing path components. */
export function copyName(path: string): string {
  const name = path.split(/[\\/]/).pop()!.replace(/[<>:"/\\|?*\x00-\x1f]/g, "_").replace(/[. ]+$/g, "");
  if (!name || /^\.+$/.test(name)) return "file";
  return /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\.|$)/i.test(name) ? `_${name}` : name;
}
/** Reject files the operating system may execute when opened. */
export function runsWhenOpened(name: string): boolean {
  return /\.(exe|com|bat|cmd|ps1|psm1|vbs|vbe|js|jse|wsf|wsh|msi|msp|scr|pif|cpl|hta|lnk|url|reg|jar|app|command|sh|desktop|appimage|terminal|fileloc)$/i.test(name.replace(/[. ]+$/g, ""));
}
/** Check the root metadata without following a symbolic link. */
export function safeCopyRoot(info: { directory: boolean; symlink: boolean; uid: number }, uid: number): boolean {
  return info.directory && !info.symlink && info.uid === uid;
}
/** Contents of the Windows downloaded-file alternate stream. */
export function zoneIdentifier(): string {
  return "[ZoneTransfer]\r\nZoneId=3\r\n";
}
/** Arguments for marking a macOS download without a shell. */
export function quarantineArgs(file: string, unixSeconds: number): string[] {
  return ["-w", "com.apple.quarantine", `0081;${Math.floor(unixSeconds).toString(16)};CrossAgents;`, file];
}
/** Select only our old generated directories for cleanup. */
export function staleCopies(entries: readonly { name: string; modified: number }[], now: number): string[] {
  return entries.filter((entry) => /^copy-[a-zA-Z0-9]{6}$/.test(entry.name) && entry.modified < now - DAY).map((entry) => entry.name);
}
