// 设置 → 设备的读法 (docs/API-AUTH.md §2.2): pure, tested in devices.test.ts.
//
// The hub answers `GET /v1/devices` oldest first, revoked ones included. The page keeps that
// order for devices still in use and puts the revoked ones after, faded: a revoked credential
// is history, not something to act on.

import type { Device } from "@bindings/Device";
import type { DeviceKind } from "@bindings/DeviceKind";
import type { DeviceLevel } from "@bindings/DeviceLevel";

/** How a device's kind reads: a client uses the API, a node only its own channel (B1). */
export const KIND_LABELS: Record<DeviceKind, string> = { client: "客户端", node: "节点" };

/** The three levels of a credential, in the hub's own words (docs/API-AUTH.md §3). */
export const LEVEL_LABELS: Record<DeviceLevel, string> = { read: "只读", write: "读写", admin: "管理" };

/** Whether the hub has revoked this device: still listed, no longer usable. */
export const isRevoked = (device: Device): boolean => !!device.revoked_at;

/** The list as the page shows it: devices still in use first, revoked ones after, each in the
 * hub's order. A stable partition, so a reload does not shuffle the rows. */
export function sortDevices(devices: readonly Device[]): Device[] {
  return [...devices.filter((d) => !isRevoked(d)), ...devices.filter(isRevoked)];
}
