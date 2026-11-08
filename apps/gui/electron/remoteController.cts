// Remote mode, the whole of it as main.cts sees it: which mode the GUI is in, the saved pairing,
// pairing a new device and the tunnel to the hub. Electron stays outside (the crypto, the
// windows), so tests drive it with a fake ssh and a stand-in crypto.

import {
  checkDeviceName, checkTarget, describeTarget, openToken, publicState, readRecord, removeRecord, REPAIR, sealToken,
  writeRecord, type ConnectionState, type Crypto, type PairStep, type RemoteRecord, type RemoteTarget, type TunnelState,
} from "./remote.cjs";
import { locate, pairDevice, Tunnel, type Ssh } from "./remoteTunnel.cjs";

/** The remote hub cannot be reached now: `hub:request` answers 503 with this reason. */
export class Unavailable extends Error {}

export type ControllerOptions = {
  /** `<userData>/remote.json`. */
  file: string;
  crypto: Crypto;
  ssh: Ssh;
  /** The default device name (`<host name>-gui`, cleaned). */
  defaultDeviceName: string;
  onState(state: ConnectionState): void;
  onPairStep(step: PairStep): void;
  /**
   * The hub behind `hub()` changed (mode switched, re-paired, forgotten while remote): the
   * windows reload and the event streams start again.
   */
  onHubChange(mode: "local" | "remote"): void;
  log?(line: string): void;
  /** Passed to each Tunnel (tests make them short). */
  tunnel?: { upTimeoutMs?: number; watchMs?: number; delay?(attempt: number): number };
};

export class RemoteController {
  private record: RemoteRecord | undefined;
  private tunnel: Tunnel | undefined;
  /** The tunnel's state, or why there is none (no readable token). */
  private tunnelState: TunnelState | undefined;
  private token: string | undefined;
  /** Serialises pair / use / forget, so two clicks cannot race. */
  private queue: Promise<unknown> = Promise.resolve();

  constructor(private readonly opts: ControllerOptions) {}

  /** Reads the saved pairing; in remote mode starts the tunnel. */
  async init(): Promise<void> {
    this.record = await readRecord(this.opts.file);
    if (this.record?.mode === "remote") this.connect();
  }

  mode(): "local" | "remote" {
    return this.record?.mode ?? "local";
  }

  get(): ConnectionState {
    return publicState(this.record, this.tunnelState, this.opts.defaultDeviceName);
  }

  /** The tray's line: target and whether it is connected; undefined in local mode. */
  remoteSummary(): { target: string; connected: boolean } | undefined {
    if (this.mode() !== "remote" || !this.record) return undefined;
    return { target: describeTarget(this.record.target), connected: this.tunnelState?.status === "connected" };
  }

  /** The remote hub to talk to now; throws `Unavailable` with the reason when there is none. */
  hub(): { base: string; token: string } {
    if (this.mode() !== "remote") throw new Unavailable("不在远程模式");
    const base = this.tunnel?.base();
    if (!base || !this.token) {
      const state = this.tunnelState;
      const why = state?.reason ?? (state?.status === "connecting" ? "正在连接中心" : "没有连上中心");
      throw new Unavailable(`远程 hub 暂时连不上：${why}`);
    }
    return { base, token: this.token };
  }

  /** A request in remote mode got 401: the device was revoked. */
  unauthorized(): void {
    if (this.mode() === "remote") this.tunnel?.unauthorized();
  }

  pair(target: unknown, deviceName: unknown): Promise<ConnectionState> {
    return this.serial(() => this.doPair(target, deviceName));
  }

  use(mode: unknown): Promise<ConnectionState> {
    return this.serial(() => this.doUse(mode));
  }

  reconnect(): Promise<ConnectionState> {
    return this.serial(async () => {
      if (this.mode() !== "remote") return this.get();
      if (!this.tunnel || !this.token) this.connect();
      else this.tunnel.reconnect();
      return this.get();
    });
  }

  /** Forgets the pairing here only; the device stays on the hub until revoked there. */
  forget(): Promise<ConnectionState> {
    return this.serial(async () => {
      const wasRemote = this.mode() === "remote";
      await this.disconnect();
      await removeRecord(this.opts.file);
      this.record = undefined;
      this.publish();
      if (wasRemote) this.opts.onHubChange("local");
      return this.get();
    });
  }

  /** Ends the tunnel (GUI quitting). */
  async dispose(): Promise<void> {
    await this.disconnect();
  }

  killNow(): void {
    this.tunnel?.killNow();
  }

  private serial<T>(work: () => Promise<T>): Promise<T> {
    const next = this.queue.then(work, work);
    this.queue = next.catch(() => {});
    return next;
  }

  private publish() {
    this.opts.onState(this.get());
  }

  private async doPair(rawTarget: unknown, rawName: unknown): Promise<ConnectionState> {
    const target = checkTarget(rawTarget);
    const deviceName = checkDeviceName(rawName);
    // Refuse before a token is issued that could not be kept.
    const why = this.opts.crypto.unavailable();
    if (why) throw new Error(`无法安全保存 token：${why}。没有配对。`);
    this.opts.onPairStep("locate");
    const hub = await locate(this.opts.ssh, target);
    this.opts.onPairStep("pair");
    const paired = await pairDevice(this.opts.ssh, target, hub, deviceName);
    const sealed = sealToken(this.opts.crypto, paired.token);
    const sameTarget = this.record && describeTarget(this.record.target) === describeTarget(target);
    const record: RemoteRecord = {
      mode: "remote",
      target,
      deviceName: paired.deviceName || deviceName,
      deviceId: paired.deviceId,
      pairedAt: new Date().toISOString(),
      ...(sameTarget && this.record?.localPort ? { localPort: this.record.localPort } : {}),
      token: sealed,
    };
    await this.disconnect();
    await writeRecord(this.opts.file, record);
    this.record = record;
    this.opts.log?.(`remote: paired ${record.deviceName} (${record.deviceId}) with ${describeTarget(target)}`);
    this.opts.onPairStep("tunnel");
    this.connect();
    await this.settled();
    this.opts.onHubChange("remote");
    return this.get();
  }

  private async doUse(raw: unknown): Promise<ConnectionState> {
    if (raw !== "local" && raw !== "remote") throw new Error(`unknown mode: ${String(raw)}`);
    const mode: "local" | "remote" = raw;
    if (mode === this.mode()) return this.get();
    if (mode === "remote" && !this.record) throw new Error("还没有配对远程 hub");
    const record = { ...this.record!, mode };
    await writeRecord(this.opts.file, record);
    this.record = record;
    if (mode === "remote") this.connect();
    else await this.disconnect();
    this.publish();
    this.opts.onHubChange(mode);
    return this.get();
  }

  /** Starts the tunnel of the saved pairing, or stops at "请重新配对" without a readable token. */
  private connect() {
    const record = this.record;
    if (!record) return;
    void this.tunnel?.close();
    this.tunnel = undefined;
    this.token = openToken(this.opts.crypto, record.token);
    if (!this.token) {
      this.tunnelState = { status: "offline", reason: REPAIR, stopped: true };
      this.publish();
      return;
    }
    const tunnel: Tunnel = new Tunnel({
      ssh: this.opts.ssh,
      target: record.target,
      token: this.token,
      localPort: record.localPort,
      onLocalPort: (port) => {
        if (this.record !== record) return;
        record.localPort = port;
        void writeRecord(this.opts.file, record).catch((err: unknown) => this.opts.log?.(`remote: cannot save the local port: ${String(err)}`));
      },
      onState: (state) => {
        if (this.tunnel !== tunnel) return;
        this.tunnelState = state;
        this.publish();
      },
      onCheck: () => this.opts.onPairStep("check"),
      log: this.opts.log,
      ...this.opts.tunnel,
    });
    this.tunnel = tunnel;
    this.tunnelState = undefined;
    tunnel.start();
  }

  private async disconnect() {
    const tunnel = this.tunnel;
    this.tunnel = undefined;
    this.token = undefined;
    this.tunnelState = undefined;
    await tunnel?.close();
  }

  /** Waits until the tunnel is connected or has failed once. */
  private settled(): Promise<void> {
    const done = () => this.tunnelState?.status === "connected" || this.tunnelState?.status === "offline" || !this.tunnel;
    if (done()) return Promise.resolve();
    return new Promise((resolve) => {
      const timer = setInterval(() => {
        if (done()) {
          clearInterval(timer);
          resolve();
        }
      }, 50);
    });
  }
}

export type { ConnectionState, PairStep, RemoteTarget };
