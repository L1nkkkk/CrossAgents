// An in-memory `bridge.connection` for the mock preview (`npm run preview:mock`) and page tests.
// Whether pairing works, why it fails and whether the tunnel drops are set by the options.

import type { ConnectionState, CrossAgentsBridge, PairStep, RemoteTarget } from "../bridge";

export type MockConnectionOptions = {
  bundledHub?: boolean;
  /** Start in remote mode, paired with this target. */
  paired?: RemoteTarget;
  /** Pairing fails with this reason (as ssh's stderr would read). */
  pairError?: string;
  /** The tunnel drops this many ms after connecting, with `dropReason`. */
  dropAfterMs?: number;
  dropReason?: string;
  /** The tunnel stops for good after connecting: the device was revoked. */
  revoked?: boolean;
  /** The tunnel never comes up: it answers `offline` with this reason. `retry` keeps trying. */
  offline?: { reason: string; retry?: boolean };
  /** How long each pairing step takes. */
  stepMs?: number;
  defaultDeviceName?: string;
};

export type MockConnection = NonNullable<CrossAgentsBridge["connection"]>;

const REVOKED = "这台设备已被吊销或 token 失效，请重新配对";

/** The mock `connection`; `#connect-page` plugs it into MockBridge. */
export function createMockConnection(options: MockConnectionOptions = {}): MockConnection {
  const stepMs = options.stepMs ?? 300;
  const stateListeners = new Set<(state: ConnectionState) => void>();
  const stepListeners = new Set<(step: PairStep) => void>();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let state: ConnectionState = { mode: "local", bundledHub: options.bundledHub ?? true, defaultDeviceName: options.defaultDeviceName ?? "mock-pc-gui" };
  if (options.paired) {
    state = {
      ...state,
      mode: "remote",
      remote: { target: options.paired, deviceName: state.defaultDeviceName, deviceId: "dev-mock", pairedAt: new Date().toISOString() },
      tunnel: { status: "connecting" },
    };
  }

  const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));
  const copy = (): ConnectionState => structuredClone(state);
  const set = (next: ConnectionState) => {
    state = next;
    for (const listener of stateListeners) listener(copy());
  };
  const step = (s: PairStep) => {
    for (const listener of stepListeners) listener(s);
  };

  /** Brings the tunnel up, then drops or stops it as the options say. */
  const connect = () => {
    if (timer) clearTimeout(timer);
    set({ ...state, tunnel: { status: "connecting" } });
    timer = setTimeout(() => {
      if (state.mode !== "remote") return;
      if (options.offline) {
        set({ ...state, tunnel: { status: "offline", reason: options.offline.reason, stopped: !options.offline.retry } });
        if (options.offline.retry) timer = setTimeout(connect, 1_000);
        return;
      }
      set({ ...state, tunnel: { status: "connected", localPort: 50123, remotePort: 7420 } });
      if (options.revoked) {
        timer = setTimeout(() => set({ ...state, tunnel: { ...state.tunnel!, status: "offline", reason: REVOKED, stopped: true } }), stepMs);
      } else if (options.dropAfterMs !== undefined) {
        timer = setTimeout(() => {
          set({ ...state, tunnel: { ...state.tunnel!, status: "offline", reason: options.dropReason ?? "ssh 退出了（退出码 255）" } });
          timer = setTimeout(connect, 1_000);
        }, options.dropAfterMs);
      }
    }, stepMs);
  };
  const disconnect = () => {
    if (timer) clearTimeout(timer);
    timer = undefined;
  };
  if (state.mode === "remote") connect();

  return {
    get: async () => copy(),
    async pair(target, deviceName) {
      if (!target.host || target.host.startsWith("-")) throw new Error("主机只能包含字母、数字、. _ -，IPv6 地址请用方括号");
      step("locate");
      await wait(stepMs);
      step("pair");
      await wait(stepMs);
      if (options.pairError) throw new Error(options.pairError);
      disconnect();
      state = {
        ...state,
        mode: "remote",
        remote: { target: { ...target }, deviceName, deviceId: "dev-mock", pairedAt: new Date().toISOString() },
      };
      step("tunnel");
      connect();
      await wait(stepMs);
      step("check");
      return copy();
    },
    async use(mode) {
      if (mode === "remote" && !state.remote) throw new Error("还没有配对远程 hub");
      if (mode === "local") {
        disconnect();
        set({ ...state, mode, tunnel: undefined });
      } else {
        state = { ...state, mode };
        connect();
      }
      return copy();
    },
    async reconnect() {
      if (state.mode === "remote") connect();
      return copy();
    },
    async forget() {
      disconnect();
      set({ mode: "local", bundledHub: true, defaultDeviceName: state.defaultDeviceName });
      return copy();
    },
    onState(listener) {
      stateListeners.add(listener);
      return () => stateListeners.delete(listener);
    },
    onPairStep(listener) {
      stepListeners.add(listener);
      return () => stepListeners.delete(listener);
    },
  };
}
