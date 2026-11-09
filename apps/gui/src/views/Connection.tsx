// 设置 → 连接 (docs/MULTI-NODE.md §4.3, §14): which hub this window talks to, pairing a hub on
// another machine over SSH, and switching back. The token never reaches this page: pairing runs
// in the main process (`bridge.connection`), and a switch reloads the window.

import { useEffect, useState, type FormEvent } from "react";
import type { ConnectionState, CrossAgentsBridge, PairStep, RemoteTarget } from "../bridge";
import { errorText } from "../api";
import { ErrorBox, Field } from "../components";
import { PAIR_STEPS, connectionWords, pairFailure, stepIndex, stepWords, targetWords, type PairFailure } from "../lib/connection";
import { useConnectionState } from "../lib/useConnection";
import { useArmed } from "../parts";
import { when } from "../lib/time";
import type { TailscaleStatus } from "../bridge";
import { OsIcon } from "../osIcon";
import { peerAddress, sortPeers, tailscaleNote } from "../lib/tailscale";

export function ConnectionPage() {
  const bridge = window.crossagents?.connection;
  const state = useConnectionState();
  // A plain browser tab has no main process: no token storage, no SSH, so no remote hub.
  if (!bridge) {
    return (
      <section className="settings-section">
        <p className="settings-lead">
          这个页面开在浏览器里，只能连同地址上的 hub，也只能连本机。配对远程中心要用桌面版：设备 token 保存在桌面版的主进程里，SSH 隧道也由它维持。
        </p>
      </section>
    );
  }
  if (!state) return <p className="settings-lead">正在读取连接状态…</p>;
  return <ConnectionForm bridge={bridge} state={state} />;
}

function ConnectionForm({ bridge, state }: { bridge: NonNullable<CrossAgentsBridge["connection"]>; state: ConnectionState }) {
  const words = connectionWords(state);
  const paired = state.remote;
  const [host, setHost] = useState("");
  const [user, setUser] = useState("");
  const [port, setPort] = useState("");
  const [name, setName] = useState(state.defaultDeviceName);
  const [command, setCommand] = useState("");
  const [advanced, setAdvanced] = useState(false);
  const [busy, setBusy] = useState(false);
  const [step, setStep] = useState<PairStep>();
  const [failure, setFailure] = useState<{ error: string; next: PairFailure }>();
  const [actionError, setActionError] = useState<string>();
  const forget = useArmed();

  // The device name the main process suggests can arrive with the first state.
  useEffect(() => {
    setName((current) => current || state.defaultDeviceName);
  }, [state.defaultDeviceName]);

  const target: RemoteTarget = {
    host: host.trim(),
    ...(user.trim() ? { user: user.trim() } : {}),
    ...(port.trim() ? { sshPort: Number(port) } : {}),
    ...(command.trim() ? { command: command.trim() } : {}),
  };

  async function pair(e: FormEvent) {
    e.preventDefault();
    if (!target.host || busy) return;
    setBusy(true);
    setFailure(undefined);
    setStep(undefined);
    const off = bridge.onPairStep(setStep);
    try {
      // On success the main process switches mode and reloads this window, so there is no
      // state to keep here; a failure comes back as ssh's own words.
      await bridge.pair(target, name.trim() || state.defaultDeviceName);
    } catch (err) {
      const error = errorText(err);
      setFailure({ error, next: pairFailure(error, target) });
    } finally {
      off();
      setStep(undefined);
      setBusy(false);
    }
  }

  async function act(run: () => Promise<unknown>) {
    setBusy(true);
    setActionError(undefined);
    try {
      await run();
    } catch (err) {
      setActionError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  const index = stepIndex(step);
  return (
    <>
      <TailscaleSection onUse={setHost} disabled={busy} />
      <section className="settings-section">
        <div className="settings-head">
          <h2>现在连的是</h2>
        </div>
        <div className="setting-card">
          <div className="setting-line">
            <div>
              <div className="setting-label">{words.label}</div>
              <div className="setting-hint">
                {words.mode === "local" ? (state.bundledHub ? "本机的 hub：它就在这台电脑上，退出 GUI 也照常运行。" : "这个安装包不带 hub，只能连接另一台电脑上的 hub") : `配对的设备：${paired?.deviceName}${paired ? ` · 配对于 ${when(paired.pairedAt)}` : ""}`}
              </div>
              {words.detail && <div className="setting-hint">{words.detail}</div>}
            </div>
            <span className={`chip small ${words.tone === "danger" ? "danger" : words.tone === "warn" ? "warn" : words.tone === "ok" ? "ok" : ""}`}>{words.status}</span>
          </div>
        </div>
      </section>

      <section className="settings-section">
        <div className="settings-head">
          <h2>{paired ? "配对另一台中心" : "配对一台远程中心"}</h2>
        </div>
        <p className="settings-lead">
          用 SSH 登录中心的这台电脑，在它上面签发一台本机设备，再把中心的回环端口转到本机。认证交给 ~/.ssh/config 与 ssh-agent，不在这里输口令。
        </p>
        <div className="setting-card">
          <form className="form conn-form" onSubmit={pair}>
            <div className="conn-row">
              <Field label="主机（必填）">
                <input data-field="host" value={host} onChange={(e) => setHost(e.target.value)} placeholder="centre.example 或 192.0.2.10" required disabled={busy} autoComplete="off" />
              </Field>
              <Field label="用户名">
                <input data-field="user" value={user} onChange={(e) => setUser(e.target.value)} placeholder="不填就用 ssh 的默认用户" disabled={busy} autoComplete="off" />
              </Field>
              <Field label="SSH 端口">
                <input data-field="port" value={port} onChange={(e) => setPort(e.target.value.replace(/[^0-9]/g, ""))} placeholder="22" inputMode="numeric" disabled={busy} />
              </Field>
            </div>
            <div className="conn-row">
              <Field label="设备名（在中心的「设备」页里认它）">
                <input data-field="name" value={name} onChange={(e) => setName(e.target.value)} maxLength={64} disabled={busy} />
              </Field>
            </div>
            <label className="field-check">
              <input type="checkbox" checked={advanced} onChange={(e) => setAdvanced(e.target.checked)} disabled={busy} />
              高级
            </label>
            {advanced && (
              <Field label="中心上的 crossagents 命令">
                <input data-field="command" value={command} onChange={(e) => setCommand(e.target.value)} placeholder="留空就用 PATH 里的 crossagents" disabled={busy} autoComplete="off" />
              </Field>
            )}
            {busy && (
              <div className="conn-steps" role="status">
                <div>{stepWords(step) || "正在开始…"}</div>
                <ol>
                  {PAIR_STEPS.map((s, i) => (
                    <li key={s.key} className={i < index ? "done" : i === index ? "now" : ""}>
                      {s.label}
                    </li>
                  ))}
                </ol>
              </div>
            )}
            {failure && (
              <>
                <ErrorBox error={failure.error} />
                <div className="notice">
                  <strong>{failure.next.title}</strong>
                  <div className="setting-hint">{failure.next.next}</div>
                </div>
              </>
            )}
            <div className="row">
              <button className="primary" type="submit" data-action="pair" disabled={busy || !host.trim()}>
                {busy ? "配对中…" : "配对并连接"}
              </button>
              {busy && <span className="setting-hint">配对时要登录中心几次，可能要十几秒。</span>}
            </div>
          </form>
        </div>
        <div className="notice">
          配对拿到的是 <strong>write（读写）</strong> 设备：write 能让 agent 在中心上执行命令（agent 默认自动执行），所以只配对你自己的机器。开终端、批准扩展、改 agent 权限要 <strong>admin（管理）</strong> 级别，write 设备在这些入口会被 hub 拒绝。
        </div>
      </section>

      {paired && (
        <section className="settings-section">
          <div className="settings-head">
            <h2>这台中心</h2>
          </div>
          <div className="setting-card">
            <div className="setting-line">
              <div>
                <div className="setting-label">所在机器</div>
                <div className="setting-hint mono">{targetWords(paired.target)}</div>
              </div>
            </div>
            <div className="setting-line">
              <div>
                <div className="setting-label">切回本机</div>
                <div className="setting-hint">{state.bundledHub ? "换回这台电脑上的 hub。切换会整页重新加载。" : "这个安装包不带 hub，只能连接另一台电脑上的 hub"}</div>
              </div>
              <button disabled={busy || state.mode === "local" || !state.bundledHub} onClick={() => void act(() => bridge.use("local"))}>
                切回本机
              </button>
            </div>
            <div className="setting-line">
              <div>
                <div className="setting-label">连到 {targetWords(paired.target)}</div>
                <div className="setting-hint">换回这台中心。切换会整页重新加载。</div>
              </div>
              <button disabled={busy || state.mode === "remote"} onClick={() => void act(() => bridge.use("remote"))}>
                连到 {targetWords(paired.target)}
              </button>
            </div>
            <div className="setting-line">
              <div>
                <div className="setting-label">重新连接</div>
                <div className="setting-hint">隧道断了、或你刚在中心的「设备」页放行了它，用这个立刻再试一次。</div>
              </div>
              <button disabled={busy || state.mode === "local"} onClick={() => void act(() => bridge.reconnect())}>
                重新连接
              </button>
            </div>
            <div className="setting-line">
              <div>
                <div className="setting-label">忘记这台中心</div>
                <div className="setting-hint">
                  只删掉这台电脑上的配对，<strong>不会吊销</strong>中心上的设备。要吊销，去中心设置里的「设备」页，或在中心运行 <code>crossagents device revoke {paired.deviceName}</code>。
                </div>
              </div>
              <button
                type="button"
                className={`ghost small danger ${forget.armed ? "armed" : ""}`}
                data-action="forget"
                disabled={busy}
                onClick={() => forget.click(() => void act(() => bridge.forget()))}
              >
                {forget.armed ? "再点一次忘记" : "忘记"}
              </button>
            </div>
          </div>
          {actionError && <ErrorBox error={actionError} />}
        </section>
      )}
    </>
  );
}

const TS_TITLES = { missing: "没装 Tailscale", "needs-login": "Tailscale 需要登录", stopped: "Tailscale 没在运行", running: "" } as const;

/** The machines on this person's Tailscale network, and a way to fill one in as the host. */
function TailscaleSection({ onUse, disabled }: { onUse: (address: string) => void; disabled: boolean }) {
  const bridge = window.crossagents?.tailscale;
  const [status, setStatus] = useState<TailscaleStatus>();
  const [error, setError] = useState<string>();
  const [loading, setLoading] = useState(false);
  const refresh = () => {
    if (!bridge) return;
    setLoading(true);
    setError(undefined);
    bridge
      .status()
      .then(setStatus, (e) => setError(errorText(e)))
      .finally(() => setLoading(false));
  };
  useEffect(refresh, [bridge]);
  if (!bridge) return null;
  const note = status ? tailscaleNote(status) : undefined;
  return (
    <section className="settings-section" data-section="tailscale">
      <div className="settings-head">
        <h2>Tailscale</h2>
        <button type="button" className="ghost small" data-action="tailscale-refresh" disabled={loading} onClick={refresh}>
          {loading ? "读取中…" : "刷新"}
        </button>
      </div>
      <p className="settings-lead">同一个 Tailscale 账号下的机器可以直接互相访问：选一台，把它的地址填进下面的主机栏。</p>
      <ErrorBox error={error} />
      <div className="setting-card">
        {!status ? (
          <div className="setting-line">
            <div className="setting-hint">{error ? "读不到 Tailscale 状态。" : "正在读取 Tailscale…"}</div>
          </div>
        ) : note ? (
          <div className="setting-line">
            <div>
              <div className="setting-label">{TS_TITLES[status.state]}</div>
              <div className="setting-hint">{note}</div>
            </div>
          </div>
        ) : !status.peers.length ? (
          <div className="setting-line">
            <div className="setting-hint">这个账号下没有别的机器。在另一台机器上装 Tailscale 并登录同一个账号。</div>
          </div>
        ) : (
          sortPeers(status.peers).map((peer) => {
            const address = peerAddress(peer);
            const away = peer.online ? "" : peer.lastSeen ? `离线 · ${when(peer.lastSeen)}` : "离线";
            return (
              <div key={peer.dnsName ?? peer.hostName} className="setting-line ts-peer" style={peer.online ? undefined : { opacity: 0.75 }}>
                <div className="device-name">
                  <div className="setting-label os-line">
                    <OsIcon os={peer.os} size={15} />
                    {peer.hostName}
                    <span className={`dot ${peer.online ? "ok" : ""}`} title={peer.online ? "在线" : away} />
                    {away && <span className="setting-hint">{away}</span>}
                  </div>
                  <div className="setting-hint mono ts-addr">{[peer.dnsName, peer.ips.find((ip) => ip.startsWith("100."))].filter(Boolean).join(" · ") || "没有地址"}</div>
                </div>
                <button type="button" data-action="use-address" disabled={disabled || !address} title={address ? `主机栏填 ${address}` : "这台机器没有可用的地址"} onClick={() => address && onUse(address)}>
                  用这个地址
                </button>
              </div>
            );
          })
        )}
      </div>
    </section>
  );
}
