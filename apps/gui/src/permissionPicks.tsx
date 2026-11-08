import type { RefObject } from "react";
import type { Agent } from "@bindings/Agent";
import type { PermissionPolicy } from "@bindings/PermissionPolicy";
import { api } from "./api";
import { useOperation } from "./lib/features";
import { CURSOR_GUARD_REFUSED, PERMISSION_OPTIONS, cursorCannotGuard, permissionHint, type PermissionChoice } from "./lib/permission";

/** `config.setting_sources`, kept as it is when only the permission changes. */
export function settingSourcesOf(agent: Pick<Agent, "config">): string | null {
  const value = (agent.config as Record<string, unknown> | null | undefined)?.setting_sources;
  return typeof value === "string" ? value : null;
}

/**
 * Stores the choice through `POST …/settings` — the same request the Agents editor sends:
 * the agent's own setting_sources (or null) rides along, so nothing else changes.
 */
export async function applyPermission(agent: Pick<Agent, "id" | "config">, choice: PermissionChoice): Promise<Agent> {
  return api.agentSettings(agent.id, { setting_sources: settingSourcesOf(agent), permission: choice });
}

/**
 * 「危险操作怎么处理」, shared by the Agents editor, the start page and a planned task
 * (docs/tasks/perm-hub). `stack` is the editor's radio list with each choice's line; `compact`
 * is a select for the places that only have room for a chip. Cursor on Windows cannot ask
 * before each action, so 「要我确认」 is disabled there with the hub's own reason.
 */
export function PermissionPicks({
  value,
  onChange,
  plugin,
  variant = "stack",
  label = "权限",
  disabled = false,
  groupRef,
  radioName = "permission",
}: {
  value: PermissionChoice;
  onChange: (choice: PermissionChoice) => void;
  /** The agent (or the plugin a new agent would run on): Cursor on Windows cannot guard. */
  plugin?: string;
  variant?: "stack" | "compact";
  label?: string;
  disabled?: boolean;
  groupRef?: RefObject<HTMLDivElement | null>;
  radioName?: string;
}) {
  const blocked = !!plugin && cursorCannotGuard({ plugin });
  // Since #retier the setting is admin (access.rs): a write device's 403 greys the picker and
  // says which level is missing, instead of failing when the form is saved.
  const settings = useOperation("agent.settings");
  const denied = settings.refused ? `改 agent 的设置需要「管理」（admin）级别，这个凭据不够。hub 说：${settings.reason}` : undefined;
  const off = disabled || !!denied;
  if (variant === "compact") {
    return (
      <span className="permission-compact">
        <select
          aria-label={label}
          title={denied ?? (blocked ? CURSOR_GUARD_REFUSED : permissionHint(value))}
          value={value ?? ""}
          disabled={off}
          onChange={(e) => onChange(e.target.value === "" ? null : (e.target.value as PermissionPolicy))}
        >
          {PERMISSION_OPTIONS.map((option) => {
            const cursorOff = option.value === "guard" && blocked;
            return (
              <option
                key={option.value ?? "default"}
                value={option.value ?? ""}
                disabled={cursorOff}
                title={cursorOff ? CURSOR_GUARD_REFUSED : option.hint}
              >
                {cursorOff ? `${option.short}（Cursor 不能选）` : option.short}
              </option>
            );
          })}
        </select>
        {/* The reason is visible, not only in the tooltip: a greyed choice alone looks broken. */}
        {denied ? (
          <span className="setting-hint" title={denied}>
            要 admin 级别，这里改不了
          </span>
        ) : (
          blocked && (
            <span className="setting-hint" title={CURSOR_GUARD_REFUSED}>
              Windows 上的 Cursor 不能选「要我确认」
            </span>
          )
        )}
      </span>
    );
  }
  return (
    <div className="permission-picks" role="radiogroup" aria-label={label} ref={groupRef} style={{ display: "flex", flexDirection: "column", gap: 6, marginTop: 2 }}>
      {denied && (
        <div className="setting-hint" title={denied} style={{ marginTop: 0 }}>
          {denied}
        </div>
      )}
      {PERMISSION_OPTIONS.map((option) => {
        const cursorOff = option.value === "guard" && blocked;
        const why = cursorOff ? CURSOR_GUARD_REFUSED : option.hint;
        return (
          <label
            key={option.value ?? "default"}
            className="field-check"
            title={why}
            aria-disabled={cursorOff || off || undefined}
            style={{ alignItems: "flex-start", ...(cursorOff || off ? { opacity: 0.55, cursor: "not-allowed" } : null) }}
          >
            <input
              type="radio"
              name={radioName}
              checked={value === option.value}
              disabled={cursorOff || off}
              onChange={() => onChange(option.value)}
            />
            <span style={{ display: "flex", flexDirection: "column", gap: 1 }}>
              <span>{option.label}</span>
              <span className="setting-hint" style={{ marginTop: 0 }}>
                {why}
              </span>
            </span>
          </label>
        );
      })}
    </div>
  );
}
