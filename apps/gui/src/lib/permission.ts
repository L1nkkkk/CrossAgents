// What an agent may do without asking (crates/core/src/driver.rs `PermissionPolicy`, set through
// `POST /v1/agents/{key}/settings`): its own choice, or none — follow the run, else the default.

import type { PermissionPolicy } from "@bindings/PermissionPolicy";

/** The agent's own permission; `null` follows the run / the plugin's default. */
export type PermissionChoice = PermissionPolicy | null;

/** The four choices as the settings form lists them, each with its one line. */
export const PERMISSION_OPTIONS: { value: PermissionChoice; label: string; hint: string }[] = [
  { value: null, label: "跟随 run / 默认", hint: "不单独设置：run 要求确认时就确认，其余时候按插件的默认" },
  { value: "guard", label: "危险操作要我确认", hint: "危险命令或改动都先问过你，再决定做不做" },
  { value: "bypass", label: "自动执行", hint: "危险操作直接执行，不再问你" },
  { value: "deny", label: "拒绝", hint: "危险操作一律拒绝，它只能做安全的" },
];

/** `config.permission`, or null when it was never set (or holds something unknown). */
export function permissionOf(agent: { config?: unknown }): PermissionChoice {
  const value = (agent.config as Record<string, unknown> | null | undefined)?.permission;
  return value === "guard" || value === "bypass" || value === "deny" ? value : null;
}

/** Whether this machine is Windows, which is where Cursor cannot ask before each action. */
export function onWindows(): boolean {
  const nav = typeof navigator === "undefined" ? undefined : navigator;
  if (!nav) return false;
  return /^Win/i.test(nav.platform ?? "") || /Windows/i.test(nav.userAgent ?? "");
}

/**
 * Cursor has no permission prompt on Windows (crates/orchestrator/src/approval.rs
 * `unusable_on_platform`), so the hub refuses to set it to guard there. The GUI greys that
 * choice out with the hub's own sentence instead of letting the save fail.
 */
export const CURSOR_GUARD_REFUSED = "Cursor cannot ask before each action on Windows; set this agent's permission to 自动执行 (bypass)";

/** True for a Cursor agent on this Windows machine: 「危险操作要我确认」 is not available. */
export function cursorCannotGuard(agent: { plugin?: string }, windows = onWindows()): boolean {
  return windows && agent.plugin === "cursor";
}
