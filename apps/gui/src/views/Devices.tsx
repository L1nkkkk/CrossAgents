// 设置 → 设备 (docs/API-AUTH.md §2.2, §4): the credentials the hub has issued, and revoking
// one. Revoked devices stay in the list, faded and last, with when they were revoked. A
// credential too weak to revoke greys the button and says which level is missing (the hub's
// own 403 message); it never hides the page or the entry.

import { useState } from "react";
import type { Device } from "@bindings/Device";
import { ApiError, api, errorText } from "../api";
import { ErrorBox } from "../components";
import { useHubData } from "../hub";
import { isRevoked, KIND_LABELS, LEVEL_LABELS, sortDevices } from "../lib/devices";
import { full, when } from "../lib/time";
import { useArmed } from "../parts";

export function DevicesPage() {
  const devices = useHubData(() => api.listDevices().then((r) => r.devices), [], () => false);
  /** The hub's 403 message for revoking: the credential's level is too low. */
  const [refused, setRefused] = useState<string>();
  const [error, setError] = useState<string>();
  const list = sortDevices(devices.data ?? []);

  async function revoke(device: Device) {
    setError(undefined);
    try {
      await api.revokeDevice(device.id);
      devices.reload();
    } catch (err) {
      if (err instanceof ApiError && err.status === 403) setRefused(err.message);
      else setError(errorText(err));
    }
  }

  return (
    <>
      <p className="settings-lead">
        这些是 hub 签发过的凭据。客户端按设备的级别访问 API，节点只走节点通道；吊销后它的下一次请求会被拒绝，记录留在这里。
      </p>
      <ErrorBox error={devices.error ?? error} />
      {refused && (
        <div className="notice">
          吊销设备需要「管理」（admin）级别，这个凭据不够，按钮已置灰。hub 说：{refused}
        </div>
      )}
      {devices.data === undefined ? null : !list.length ? (
        <div className="setting-card">
          <div className="setting-line">
            <div>
              <div className="setting-label">还没有设备</div>
              <div className="setting-hint">
                在 hub 所在的机器上运行 <code>crossagents device add &lt;名字&gt;</code>；从别的电脑经 SSH 连过来要等 A2。
              </div>
            </div>
          </div>
        </div>
      ) : (
        <div className="setting-card">
          {list.map((device) => (
            <DeviceRow key={device.id} device={device} refused={refused} onRevoke={revoke} />
          ))}
        </div>
      )}
    </>
  );
}

/** One device: what it is, what it may do, when it was made and last seen; revoked ones faint. */
function DeviceRow({ device, refused, onRevoke }: { device: Device; refused?: string; onRevoke: (device: Device) => void }) {
  const remove = useArmed();
  const revoked = isRevoked(device);
  return (
    <div className={`setting-line device-row ${revoked ? "revoked" : ""}`}>
      <div className="device-name">
        <div className="setting-label">
          {device.name}
          <span className="chip small">{KIND_LABELS[device.kind]}</span>
          <span className="chip small">{LEVEL_LABELS[device.level]}</span>
          {revoked && <span className="chip small danger">已吊销</span>}
        </div>
        <div className="device-times">
          <span className="setting-hint" title={`创建于 ${full(device.created_at)}`}>
            创建 {when(device.created_at)}
          </span>
          <span className="setting-hint" title={device.last_seen_at ? `最后在线 ${full(device.last_seen_at)}` : "还没有连接过"}>
            最后在线 {device.last_seen_at ? when(device.last_seen_at) : "从未连接"}
          </span>
          {revoked && device.revoked_at && (
            <span className="setting-hint" title={`吊销于 ${full(device.revoked_at)}`}>
              吊销于 {when(device.revoked_at)}
            </span>
          )}
        </div>
      </div>
      {!revoked && (
        <button
          type="button"
          data-action="revoke"
          className={`ghost small danger ${remove.armed ? "armed" : ""}`}
          disabled={!!refused}
          title={refused ?? "这台设备的凭据立即失效；记录留着"}
          onClick={() => remove.click(() => onRevoke(device))}
        >
          {remove.armed ? "再点一次吊销" : "吊销"}
        </button>
      )}
    </div>
  );
}
