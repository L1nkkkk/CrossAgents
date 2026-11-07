import assert from "node:assert/strict";
import { test } from "node:test";
import type { Device } from "@bindings/Device";
import { isRevoked, KIND_LABELS, LEVEL_LABELS, sortDevices } from "./devices.ts";

const at = (minutes: number) => new Date(2026, 9, 22, 14, 30 - minutes).toISOString();

function device(init: Partial<Device> & { name: string }): Device {
  return {
    id: init.name,
    kind: "client",
    level: "write",
    created_at: at(60),
    last_seen_at: at(5),
    ...init,
  };
}

test("devices still in use keep the hub's order; revoked ones come after", () => {
  const old = device({ name: "旧笔记本", revoked_at: at(10) });
  const desktop = device({ name: "工作站" });
  const node = device({ name: "build-node", kind: "node", level: "read" });
  const revokedLast = device({ name: "上个月的机器", revoked_at: at(1) });

  // The hub orders by created_at, oldest first, and mixes revoked ones in.
  assert.deepEqual(sortDevices([old, desktop, node, revokedLast]).map((d) => d.name), ["工作站", "build-node", "旧笔记本", "上个月的机器"]);
});

test("a device with no revoke time is still usable", () => {
  assert.equal(isRevoked(device({ name: "a" })), false);
  assert.equal(isRevoked(device({ name: "b", revoked_at: at(1) })), true);
  // An empty list stays empty.
  assert.deepEqual(sortDevices([]), []);
});

test("kind and level read in the page's words", () => {
  assert.equal(KIND_LABELS.client, "客户端");
  assert.equal(KIND_LABELS.node, "节点");
  assert.equal(LEVEL_LABELS.read, "只读");
  assert.equal(LEVEL_LABELS.write, "读写");
  assert.equal(LEVEL_LABELS.admin, "管理");
});
