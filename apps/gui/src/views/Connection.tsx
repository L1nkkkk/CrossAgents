// 设置 → 连接 (docs/MULTI-NODE.md §4.3, §14): which hub this window talks to, and two guided ways
// to join two machines: this one is the centre (turn on the built-in hole punching and hand out
// a one-time pairing code), or this one joins a centre (paste the code, or pair over SSH). The
// token never reaches this page: pairing runs in the main process (`bridge.connection`), and a
// switch reloads the window.

import { useEffect, useRef, useState, type FormEvent } from "react";
import type { LinkInvite } from "@bindings/LinkInvite";
import type { LinkStatus } from "@bindings/LinkStatus";
import type { ConnectionState, CrossAgentsBridge, PairStep, RemoteTarget } from "../bridge";
import { ApiError, api, errorText } from "../api";
import { CopyButton, ErrorBox, Field } from "../components";
import { PAIR_STEPS, connectionWords, pairFailure, stepIndex, stepWords, targetWords, type PairFailure } from "../lib/connection";
import { CODE_STEPS, codeFailure, codeStepIndex, countdown, entranceWords, pathWords, previewCode, type CodeFailure } from "../lib/link";
import { useConnectionState } from "../lib/useConnection";
import { useFeatureSupport } from "../lib/features";
import { useArmed } from "../parts";
import { when } from "../lib/time";
import type { TailscaleStatus } from "../bridge";
import { OsIcon } from "../osIcon";
import { peerAddress, sortPeers, tailscaleNote } from "../lib/tailscale";

type Bridge = NonNullable<CrossAgentsBridge["connection"]>;

export function ConnectionPage() {
  const bridge = window.crossagents?.connection;
  const state = useConnectionState();
  // A plain browser tab has no main process: no token storage, no SSH, so no remote hub.
  if (!bridge) {
    return (
      <section className="settings-section">
        <p className="settings-lead">
          这个页面开在浏览器里，只能连同地址上的 hub，也只能连本机。配对远程中心要用桌面版：设备 token 保存在桌面版的主进程里，隧道也由它维持。
        </p>
      </section>
    );
  }
  if (!state) return <p className="settings-lead">正在读取连接状态…</p>;
  return <ConnectionForm bridge={bridge} state={state} />;
}

/** What the person came to do: join a centre, or let other machines join this one. */
type Goal = "join" | "host";

function ConnectionForm({ bridge, state }: { bridge: Bridge; state: ConnectionState }) {
  const paired = state.remote;
  // A machine with its own hub and nothing paired is most likely the centre.
  const canHost = state.mode === "local" && state.bundledHub;
  const [goal, setGoal] = useState<Goal>(canHost && !paired ? "host" : "join");
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string>();
  const forget = useArmed();

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

  return (
    <>
      <CurrentSection state={state} />

      <section className="settings-section" data-section="guide">
        <div className="settings-head">
          <h2>要做什么</h2>
        </div>
        <div className="guide-goals" role="tablist" aria-label="要做什么">
          <button type="button" role="tab" aria-selected={goal === "join"} className={goal === "join" ? "active" : ""} data-action="goal-join" onClick={() => setGoal("join")}>
            <span className="guide-goal-title">连到别的电脑上的中心</span>
            <span className="guide-goal-hint">这台只开界面，agent 在中心那台跑</span>
          </button>
          <button type="button" role="tab" aria-selected={goal === "host"} className={goal === "host" ? "active" : ""} data-action="goal-host" onClick={() => setGoal("host")}>
            <span className="guide-goal-title">让别的电脑连进来</span>
            <span className="guide-goal-hint">这台就是中心：生成配对码给另一台</span>
          </button>
        </div>
      </section>

      {goal === "join" ? <JoinSection bridge={bridge} state={state} /> : <HostSection state={state} />}

      {paired && (
        <section className="settings-section">
          <div className="settings-head">
            <h2>已配对的中心</h2>
          </div>
          <div className="setting-card">
            <div className="setting-line">
              <div>
                <div className="setting-label">所在机器</div>
                <div className="setting-hint mono">
                  {targetWords(paired.target)}
                  {paired.via === "link" ? " · 配对码（内置打洞）" : " · SSH 隧道"}
                </div>
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
                <div className="setting-hint">{paired.via === "link" ? "连接断了、或中心刚开机，用这个立刻再试一次。" : "隧道断了、或你刚在中心的「设备」页放行了它，用这个立刻再试一次。"}</div>
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

function CurrentSection({ state }: { state: ConnectionState }) {
  const words = connectionWords(state);
  const paired = state.remote;
  const path = paired?.via === "link" ? pathWords(state.tunnel) : undefined;
  return (
    <section className="settings-section">
      <div className="settings-head">
        <h2>现在连的是</h2>
      </div>
      <div className="setting-card">
        <div className="setting-line">
          <div>
            <div className="setting-label">{words.label}</div>
            <div className="setting-hint">
              {words.mode === "local"
                ? state.bundledHub
                  ? "本机的 hub：它就在这台电脑上，退出 GUI 也照常运行。"
                  : "这个安装包不带 hub，只能连接另一台电脑上的 hub"
                : `配对的设备：${paired?.deviceName}${paired ? ` · 配对于 ${when(paired.pairedAt)}` : ""}${paired?.via === "link" ? " · 内置打洞" : ""}`}
            </div>
            {words.detail && <div className="setting-hint">{words.detail}</div>}
          </div>
          <div className="row">
            {path && (
              <span className={`chip small ${path.word === "直连" ? "ok" : ""}`} title={path.title} data-role="path">
                {path.word}
              </span>
            )}
            <span className={`chip small ${words.tone === "danger" ? "danger" : words.tone === "warn" ? "warn" : words.tone === "ok" ? "ok" : ""}`}>{words.status}</span>
          </div>
        </div>
      </div>
    </section>
  );
}

// ---------------------------------------------------------------------------------------
// Joining a centre.

function JoinSection({ bridge, state }: { bridge: Bridge; state: ConnectionState }) {
  const [method, setMethod] = useState<"code" | "ssh">(state.remote?.via === "ssh" ? "ssh" : "code");
  const [host, setHost] = useState("");
  return (
    <>
      <section className="settings-section" data-section="join">
        <div className="settings-head">
          <h2>{state.remote ? "配对另一台中心" : "连到中心"}</h2>
          <div className="segmented small" role="tablist" aria-label="怎么连">
            <button type="button" role="tab" aria-selected={method === "code"} className={method === "code" ? "active" : ""} data-action="method-code" onClick={() => setMethod("code")}>
              配对码（推荐）
            </button>
            <button type="button" role="tab" aria-selected={method === "ssh"} className={method === "ssh" ? "active" : ""} data-action="method-ssh" onClick={() => setMethod("ssh")}>
              SSH
            </button>
          </div>
        </div>
        {method === "code" ? (
          <CodePairForm bridge={bridge} state={state} />
        ) : (
          <SshPairForm bridge={bridge} state={state} host={host} setHost={setHost} />
        )}
        <div className="notice">
          配对拿到的是 <strong>write（读写）</strong> 设备：write 能让 agent 在中心上执行命令（agent 默认自动执行），所以只配对你自己的机器。开终端、批准扩展、改 agent 权限要 <strong>admin（管理）</strong> 级别，write 设备在这些入口会被 hub 拒绝。
        </div>
      </section>
      {method === "ssh" && <TailscaleSection onUse={setHost} disabled={false} />}
    </>
  );
}

function CodePairForm({ bridge, state }: { bridge: Bridge; state: ConnectionState }) {
  const [code, setCode] = useState("");
  const [name, setName] = useState(state.defaultDeviceName);
  const [busy, setBusy] = useState(false);
  const [step, setStep] = useState<PairStep>();
  const [failure, setFailure] = useState<{ error: string; next: CodeFailure }>();
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    setName((current) => current || state.defaultDeviceName);
  }, [state.defaultDeviceName]);
  // The preview counts down while a code is in the box.
  useEffect(() => {
    if (!code.trim()) return;
    const timer = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(timer);
  }, [code]);
  const preview = previewCode(code, now);

  async function pair(e: FormEvent) {
    e.preventDefault();
    if (!preview?.ok || busy) return;
    setBusy(true);
    setFailure(undefined);
    setStep(undefined);
    const off = bridge.onPairStep(setStep);
    try {
      // On success the main process switches mode and reloads this window.
      await bridge.pairCode(code.trim(), name.trim() || state.defaultDeviceName);
    } catch (err) {
      const error = errorText(err);
      setFailure({ error, next: codeFailure(error) });
    } finally {
      off();
      setStep(undefined);
      setBusy(false);
    }
  }

  const index = codeStepIndex(step);
  return (
    <div className="setting-card">
      <form className="form conn-form" onSubmit={pair}>
        <ol className="guide-steps">
          <li>
            <div className="guide-step-title">在中心那台电脑上生成配对码</div>
            <div className="setting-hint">
              打开中心上的 CrossAgents → 设置 →「连接」→「让别的电脑连进来」，开启内置打洞后点「生成配对码」，再点「复制」。没有界面的中心，在终端运行 <code>crossagents link on</code>，再运行 <code>crossagents link invite</code>。
            </div>
          </li>
          <li>
            <div className="guide-step-title">把配对码粘贴到这里</div>
            <textarea
              data-field="code"
              className="mono code-input"
              value={code}
              onChange={(e) => setCode(e.target.value)}
              placeholder="ca1-…"
              rows={3}
              spellCheck={false}
              autoComplete="off"
              disabled={busy}
              aria-label="配对码"
            />
            {preview && (
              <div className={`setting-hint code-preview ${preview.ok ? "ok" : "bad"}`} role="status" data-role="code-preview">
                {preview.ok
                  ? `中心：${preview.host} · 还剩 ${countdown(preview.expires, now)} 过期 · ${preview.relay ? "经中继能从外网连" : "码里没有中继：只能在同一局域网里连"}${preview.lan ? ` · 带 ${preview.lan} 个直连地址` : ""}`
                  : preview.error}
              </div>
            )}
          </li>
          <li>
            <div className="guide-step-title">给这台电脑起个名字，然后连接</div>
            <div className="conn-row">
              <Field label="设备名（在中心的「设备」页里认它）">
                <input data-field="name" value={name} onChange={(e) => setName(e.target.value)} maxLength={64} disabled={busy} />
              </Field>
            </div>
          </li>
        </ol>
        {busy && (
          <div className="conn-steps" role="status">
            <div>{index >= 0 ? `第 ${index + 1} 步，共 ${CODE_STEPS.length} 步：${CODE_STEPS[index].label}` : "正在开始…"}</div>
            <ol>
              {CODE_STEPS.map((s, i) => (
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
          <button className="primary" type="submit" data-action="pair-code" disabled={busy || !preview?.ok}>
            {busy ? "连接中…" : "连接"}
          </button>
          {busy ? (
            <span className="setting-hint">打洞第一次连上可能要十几秒；打不通时会经中继。</span>
          ) : (
            <span className="setting-hint">不用 SSH、不用开端口、不用装 Tailscale；配对码只能用一次。</span>
          )}
        </div>
      </form>
    </div>
  );
}

function SshPairForm({ bridge, state, host, setHost }: { bridge: Bridge; state: ConnectionState; host: string; setHost: (host: string) => void }) {
  const [user, setUser] = useState("");
  const [port, setPort] = useState("");
  const [name, setName] = useState(state.defaultDeviceName);
  const [command, setCommand] = useState("");
  const [advanced, setAdvanced] = useState(false);
  const [busy, setBusy] = useState(false);
  const [step, setStep] = useState<PairStep>();
  const [failure, setFailure] = useState<{ error: string; next: PairFailure }>();

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

  const index = stepIndex(step);
  return (
    <>
      <p className="settings-lead">
        用 SSH 登录中心的这台电脑，在它上面签发一台本机设备，再把中心的回环端口转到本机。认证交给 ~/.ssh/config 与 ssh-agent，不在这里输口令。中心要先开好 OpenSSH Server（docs/MULTI-NODE.md §14）。
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
    </>
  );
}

// ---------------------------------------------------------------------------------------
// Being the centre.

function HostSection({ state }: { state: ConnectionState }) {
  const support = useFeatureSupport("link");
  if (state.mode === "remote") {
    return (
      <section className="settings-section" data-section="host">
        <div className="settings-head">
          <h2>让别的电脑连进来</h2>
        </div>
        <div className="notice">
          这个窗口现在连的是远程中心 {state.remote ? targetWords(state.remote.target) : ""}。要让别的电脑也连进那台中心，在中心那台电脑上打开这一页；没有界面的中心，在它的终端里运行 <code>crossagents link on</code>，再运行 <code>crossagents link invite</code>。
        </div>
      </section>
    );
  }
  if (!state.bundledHub) {
    return (
      <section className="settings-section" data-section="host">
        <div className="settings-head">
          <h2>让别的电脑连进来</h2>
        </div>
        <div className="notice">这个安装包不带 hub，这台电脑不能当中心。在跑 hub 的那台电脑上打开这一页生成配对码，再回到这里选「连到别的电脑上的中心」。</div>
      </section>
    );
  }
  if (support.known && !support.show) {
    return (
      <section className="settings-section" data-section="host">
        <div className="settings-head">
          <h2>让别的电脑连进来</h2>
        </div>
        <div className="notice">这台的 hub 版本没有内置打洞。更新 CrossAgents 后再来；在那之前可以用 SSH 方式让别的电脑连进来（docs/MULTI-NODE.md §14）。</div>
      </section>
    );
  }
  return <HostGuide />;
}

function HostGuide() {
  const [status, setStatus] = useState<LinkStatus>();
  const [invite, setInvite] = useState<LinkInvite>();
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [relay, setRelay] = useState("");
  const [advanced, setAdvanced] = useState(false);
  const [now, setNow] = useState(() => new Date());
  const relayTouched = useRef(false);

  const load = () =>
    api.linkStatus().then(
      (s) => {
        setStatus(s);
        if (!relayTouched.current) setRelay(s.custom_relay ?? "");
        if (s.custom_relay) setAdvanced(true);
      },
      (e) => setError(errorText(e)),
    );
  useEffect(() => {
    void load();
  }, []);
  // While the relay is not up yet, look again every two seconds.
  useEffect(() => {
    if (status?.state !== "starting") return;
    const timer = setInterval(() => void load(), 2000);
    return () => clearInterval(timer);
  }, [status?.state]);
  useEffect(() => {
    if (!invite) return;
    const timer = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(timer);
  }, [invite]);

  async function run<T>(work: () => Promise<T>, then: (value: T) => void) {
    setBusy(true);
    setError(undefined);
    try {
      then(await work());
    } catch (err) {
      setError(err instanceof ApiError && err.status === 403 ? `${errorText(err)}（这个窗口的凭据不到 admin：在中心本机打开这一页）` : errorText(err));
    } finally {
      setBusy(false);
    }
  }

  const turnOn = () => run(() => api.setLink({ enabled: true, relay: relay.trim() || undefined }), setStatus);
  const turnOff = () =>
    run(
      () => api.setLink({ enabled: false }),
      (s) => {
        setStatus(s);
        setInvite(undefined);
      },
    );
  const makeCode = () =>
    run(
      () => api.createLinkInvite(),
      (made) => {
        setInvite(made);
        setNow(new Date());
        void load();
      },
    );

  const words = entranceWords(status);
  const on = !!status?.enabled;
  const running = status?.state === "online" || status?.state === "starting";
  const left = invite ? countdown(new Date(invite.expires_at), now) : "";
  const expired = !!invite && !left;
  const tone = words.tone === "danger" ? "danger" : words.tone === "warn" ? "warn" : words.tone === "ok" ? "ok" : "";

  return (
    <section className="settings-section" data-section="host">
      <div className="settings-head">
        <h2>让别的电脑连进来</h2>
      </div>
      <p className="settings-lead">
        内置打洞：两台电脑先经中继找到对方，再尽量直接连通；打不通也能经中继用。不开端口、不用 SSH、不用装 Tailscale。连进来的电脑凭一次性配对码换到自己的设备 token。
      </p>
      <ErrorBox error={error} />
      <div className="setting-card">
        <ol className="guide-steps host-steps">
          <li className={status?.state === "online" ? "done" : "now"}>
            <div className="setting-line guide-line">
              <div>
                <div className="guide-step-title">
                  开启内置打洞 <span className={`chip small ${tone}`} data-role="entrance">{words.word}</span>
                </div>
                {words.detail && <div className="setting-hint">{words.detail}</div>}
                {status?.endpoint && (
                  <div className="setting-hint mono" title="这台中心的 id（公钥）：配对码里带着它，别的电脑凭它找到这里">
                    id {status.endpoint.slice(0, 16)}…
                  </div>
                )}
              </div>
              <div className="row">
                {!on ? (
                  <button className="primary" type="button" data-action="link-on" disabled={busy || !status} onClick={() => void turnOn()}>
                    开启
                  </button>
                ) : (
                  <>
                    <button type="button" data-action="link-restart" disabled={busy} onClick={() => void turnOn()} title="用现在的代理和中继设置重新开启">
                      重新开启
                    </button>
                    <button type="button" className="ghost small" data-action="link-off" disabled={busy} onClick={() => void turnOff()}>
                      关闭
                    </button>
                  </>
                )}
              </div>
            </div>
            <label className="field-check">
              <input type="checkbox" checked={advanced} onChange={(e) => setAdvanced(e.target.checked)} disabled={busy} />
              高级：自建中继
            </label>
            {advanced && (
              <Field label="中继地址（留空用 n0 的公共中继）">
                <input
                  data-field="relay"
                  value={relay}
                  onChange={(e) => {
                    relayTouched.current = true;
                    setRelay(e.target.value);
                  }}
                  placeholder="https://relay.example.com"
                  disabled={busy}
                  autoComplete="off"
                />
              </Field>
            )}
            {advanced && <div className="setting-hint">国内的云服务器上跑一个 iroh-relay，填在这里后点「{on ? "重新开启" : "开启"}」。生成的配对码会带上它，连进来的电脑也走它。</div>}
          </li>
          <li className={!running ? "" : invite && !expired ? "done" : "now"}>
            <div className="setting-line guide-line">
              <div>
                <div className="guide-step-title">生成配对码</div>
                <div className="setting-hint">
                  {!running
                    ? "先开启上一步。"
                    : status?.state === "starting"
                      ? "中继还没连上：现在生成的码只带局域网地址，同一局域网的电脑能连，外网的要等中继连上。"
                      : "一个码只能配对一台电脑，30 分钟内有效；中心的 hub 重启后作废。"}
                </div>
              </div>
              <button className={running && (!invite || expired) ? "primary" : ""} type="button" data-action="link-invite" disabled={busy || !running} onClick={() => void makeCode()}>
                {invite ? "再生成一个" : "生成配对码"}
              </button>
            </div>
          </li>
          <li className={invite && !expired ? "now" : ""}>
            <div className="guide-step-title">在另一台电脑上粘贴</div>
            {invite && !expired ? (
              <>
                <div className="code-box" data-role="invite">
                  <textarea className="mono code-input" readOnly value={invite.code} rows={3} onFocus={(e) => e.currentTarget.select()} aria-label="配对码" />
                  <CopyButton className="copy" text={invite.code} label="复制" title="复制配对码" />
                </div>
                <div className="setting-hint">
                  还剩 <strong>{left}</strong> 过期，只能用一次。在那台电脑上：打开 CrossAgents → 设置 →「连接」→「连到别的电脑上的中心」→「配对码」，粘贴后点「连接」。
                </div>
                <div className="setting-hint">配对码等于一把一次性的钥匙：只发给你自己的电脑，别贴到群里。</div>
              </>
            ) : (
              <div className="setting-hint">{expired ? "刚才的配对码已经过期，重新生成一个。" : "生成配对码后，在这里复制。"}</div>
            )}
          </li>
        </ol>
      </div>
      <div className="notice">
        连进来的电脑会出现在「设备」页：它拿到的是 write（读写）设备。在那里吊销它，它的连接立刻被拒绝。不想再让任何电脑连进来，就点「关闭」，没用掉的配对码也随之作废。
      </div>
    </section>
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
      <p className="settings-lead">同一个 Tailscale 账号下的机器可以直接互相访问：选一台，把它的地址填进上面的主机栏。</p>
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
