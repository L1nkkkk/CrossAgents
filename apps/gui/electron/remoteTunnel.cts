// Remote mode, process side (docs/MULTI-NODE.md §4.3, §14 "SSH 隧道"): runs the system's ssh
// to read the hub's `hub.json`, to pair (`device add`) and to keep `ssh -N -L` up, reconnecting
// with backoff. Which ssh runs is injected, so the tests use a fake one (fakeSsh.cts).

import { execFile, spawn, type ChildProcess } from "node:child_process";
import { createServer } from "node:net";
import {
  backoff, execArgs, LOCATE_COMMANDS, NOT_RUNNING, pairCommand, parseDeviceAdd, parseHubFile, remoteCommand,
  tunnelArgs, tunnelReduce, type Paired, type RemoteHub, type RemoteTarget, type TunnelEvent, type TunnelState,
} from "./remote.cjs";

/** The ssh to run: `program` with `prefix` before ssh's own arguments (a script in tests). */
export type Ssh = { program: string; prefix: string[] };

/** One ssh call's timeout: ConnectTimeout is 10 s, `device add` talks to the hub after that. */
const EXEC_TIMEOUT_MS = 30_000;

/** ssh failed: `stderr` is what ssh (or the command) said, untouched. */
export class SshError extends Error {
  constructor(message: string, readonly stderr: string, readonly code: number | null) {
    super(stderr ? `${message}\n${stderr}` : message);
  }
}

export function runSsh(ssh: Ssh, args: string[], timeout = EXEC_TIMEOUT_MS): Promise<string> {
  return new Promise((done, fail) => {
    execFile(ssh.program, [...ssh.prefix, ...args], { timeout, windowsHide: true, maxBuffer: 1 << 20 }, (err, stdout, stderr) => {
      if (!err) return done(String(stdout));
      const e = err as NodeJS.ErrnoException & { code?: unknown; killed?: boolean };
      const code = typeof e.code === "number" ? e.code : null;
      const why = e.code === "ENOENT" ? `找不到 ssh（${ssh.program}）` : e.killed ? "ssh 超时" : `ssh 失败（退出码 ${code ?? e.code}）`;
      fail(new SshError(why, String(stderr ?? ""), code));
    });
  });
}

/**
 * Reads the hub's `hub.json` over ssh. ssh's own failures (exit 255: host key, auth, network)
 * are reported as they are; a missing file means the hub does not run there.
 */
export async function locate(ssh: Ssh, target: RemoteTarget): Promise<RemoteHub & { locateMethod: "cat" | "type" }> {
  let last: unknown;
  for (const command of LOCATE_COMMANDS) {
    try {
      return { ...parseHubFile(await runSsh(ssh, execArgs(target, command))), locateMethod: command === LOCATE_COMMANDS[0] ? "cat" : "type" };
    } catch (err) {
      if (err instanceof SshError && (err.code === 255 || err.code === null)) throw err;
      last = err;
    }
  }
  const detail = last instanceof SshError ? last.stderr : "";
  throw new Error(detail ? `${NOT_RUNNING}\n${detail}` : NOT_RUNNING);
}

/** Runs `device add <name> --json` on the hub's machine. */
export async function pairDevice(ssh: Ssh, target: RemoteTarget, hub: RemoteHub & { locateMethod: "cat" | "type" }, deviceName: string): Promise<Paired> {
  let command = remoteCommand(target, hub);
  if (hub.locateMethod === "cat" && /^"(?:[A-Za-z]:|\\\\)/.test(command)) {
    try {
      const version = await runSsh(ssh, execArgs(target, "$PSVersionTable.PSVersion.Major"));
      if (/^\d+$/.test(version.trim())) command = `& ${command}`;
    } catch {
      // A non-PowerShell shell may reject the probe; use the normal command there.
    }
  }
  return parseDeviceAdd(await runSsh(ssh, execArgs(target, pairCommand(command, deviceName))));
}

/** A port nobody listens on now. */
export function freePort(): Promise<number> {
  return new Promise((done, fail) => {
    const server = createServer();
    server.once("error", fail);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() => (address && typeof address === "object" ? done(address.port) : fail(new Error("no port"))));
    });
  });
}

export function portFree(port: number): Promise<boolean> {
  return new Promise((done) => {
    const server = createServer();
    server.once("error", () => done(false));
    server.listen(port, "127.0.0.1", () => server.close(() => done(true)));
  });
}

/** Ends `child` and waits until it has really exited (Windows: the whole tree, forcibly). */
export async function killProcess(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise<void>((done) => child.once("exit", () => done()));
  child.kill();
  const gone = await Promise.race([exited.then(() => true), sleep(3_000).then(() => false)]);
  if (gone) return;
  if (process.platform === "win32" && child.pid) {
    await new Promise<void>((done) => execFile("taskkill", ["/pid", String(child.pid), "/T", "/F"], { windowsHide: true }, () => done()));
  } else {
    child.kill("SIGKILL");
  }
  await Promise.race([exited, sleep(3_000)]);
}

function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((done) => {
    if (signal?.aborted) return done();
    const timer = setTimeout(done, ms);
    signal?.addEventListener("abort", () => {
      clearTimeout(timer);
      done();
    }, { once: true });
  });
}

export type TunnelOptions = {
  ssh: Ssh;
  target: RemoteTarget;
  token: string;
  /** The local port used last time; kept unless it is taken. */
  localPort?: number;
  /** A new local port was chosen: remember it. */
  onLocalPort(port: number): void;
  onState(state: TunnelState): void;
  /** The forward is up and the token is about to be checked (PairStep "check"). */
  onCheck?(): void;
  log?(line: string): void;
  /** How long to wait for `/v1/health` through a new forward. */
  upTimeoutMs?: number;
  /** How often a connected tunnel is checked. */
  watchMs?: number;
  /** Delays between attempts; `backoff` by default. */
  delay?(attempt: number): number;
};

/**
 * Keeps `ssh -N -L 127.0.0.1:<local>:127.0.0.1:<hub>` running. Each attempt reads the hub's
 * `hub.json` again (its port may have changed). A token the hub refuses (401) stops it.
 */
export class Tunnel {
  private state: TunnelState | undefined;
  private child: ChildProcess | undefined;
  private closed = false;
  private loop: Promise<void> | undefined;
  /** Cuts the current wait (between attempts, or while connected) short. */
  private wake = new AbortController();
  private localPort: number | undefined;
  /** reconnect() asked to skip the next wait. */
  private retryNow = false;

  constructor(private readonly opts: TunnelOptions) {
    this.localPort = opts.localPort;
  }

  current(): TunnelState | undefined {
    return this.state;
  }

  /** Base URL of the forward while connected. */
  base(): string | undefined {
    return this.state?.status === "connected" && this.state.localPort ? `http://127.0.0.1:${this.state.localPort}` : undefined;
  }

  start(): void {
    if (this.closed || this.loop) return;
    this.loop = this.run().finally(() => {
      this.loop = undefined;
    });
  }

  /** Tries now: skips the wait, or starts again after a stop. */
  reconnect(): void {
    if (this.closed) return;
    if (this.state?.stopped) {
      // The loop is ending (or has ended): start a new one after it.
      void (this.loop ?? Promise.resolve()).then(() => this.start());
      return;
    }
    if (this.loop) {
      this.retryNow = true;
      void this.drop();
      this.wake.abort();
    } else {
      this.start();
    }
  }

  /** The hub refused the token: stop for good (until `reconnect`). */
  unauthorized(): void {
    this.emit({ type: "unauthorized" });
    this.wake.abort();
    void this.drop();
  }

  /** Ends the ssh process and the loop. */
  async close(): Promise<void> {
    this.closed = true;
    this.wake.abort();
    await this.drop();
    await this.loop;
  }

  /** Synchronous last resort when the app quits. */
  killNow(): void {
    this.closed = true;
    this.child?.kill();
  }

  private emit(event: TunnelEvent) {
    const next = tunnelReduce(this.state, event);
    if (next === this.state) return;
    this.state = next;
    this.opts.onState({ ...next });
  }

  private async drop() {
    const child = this.child;
    this.child = undefined;
    if (child) await killProcess(child);
  }

  private async run() {
    let attempt = 0;
    this.emit({ type: "connecting" });
    while (!this.closed) {
      if (this.state?.stopped && this.state.status === "offline") {
        // Revoked while connecting: a reconnect starts a new loop.
        return;
      }
      this.wake = new AbortController();
      let reason: string;
      try {
        await this.once();
        attempt = 0;
        reason = "隧道断开了";
      } catch (err) {
        reason = err instanceof Error ? err.message : String(err);
      }
      await this.drop();
      if (this.closed || this.state?.stopped) return;
      this.emit({ type: "down", reason });
      this.opts.log?.(`remote: tunnel down: ${reason}`);
      const wait = (this.opts.delay ?? backoff)(attempt++);
      // A fresh signal: the attempt that just ended may still abort its own.
      this.wake = new AbortController();
      if (!this.retryNow) await sleep(wait, this.wake.signal);
      this.retryNow = false;
      if (this.closed) return;
      this.emit({ type: "connecting" });
    }
  }

  /** One connection: resolves when it ends normally, rejects with why it failed. */
  private async once(): Promise<void> {
    const { ssh, target, token } = this.opts;
    const wake = this.wake;
    const hub = await locate(ssh, target);
    let port = this.localPort;
    if (port === undefined || !(await portFree(port))) {
      port = await freePort();
      this.localPort = port;
      this.opts.onLocalPort(port);
    }
    if (this.closed) return;
    const child = spawn(ssh.program, [...ssh.prefix, ...tunnelArgs(target, port, hub.port)], { windowsHide: true, stdio: ["ignore", "ignore", "pipe"] });
    this.child = child;
    let stderr = "";
    child.stderr?.setEncoding("utf8");
    child.stderr?.on("data", (chunk: string) => {
      stderr = (stderr + chunk).slice(-8_000);
    });
    const exited = new Promise<string>((done) => {
      child.once("error", (err) => done(`无法运行 ssh（${ssh.program}）：${err.message}`));
      child.once("exit", (code) => done(`ssh 退出了（退出码 ${code}）${stderr ? `\n${stderr}` : ""}`));
    });
    let ended: string | undefined;
    void exited.then((why) => {
      ended = why;
      wake.abort();
    });
    const base = `http://127.0.0.1:${port}`;

    // Up: the hub answers /v1/health through the forward.
    const deadline = Date.now() + (this.opts.upTimeoutMs ?? 20_000);
    for (;;) {
      if (ended !== undefined) throw new Error(ended);
      if (this.closed) return;
      if (await healthy(base)) break;
      if (Date.now() > deadline) throw new Error(`隧道建立后中心的 hub 没有应答${stderr ? `\n${stderr}` : ""}`);
      await sleep(250, wake.signal);
    }
    this.opts.onCheck?.();
    const info = await fetch(`${base}/v1/info`, { headers: { authorization: `Bearer ${token}` }, signal: AbortSignal.timeout(10_000) })
      .catch((err: unknown) => { throw new Error(`检查 token 失败：${err instanceof Error ? err.message : String(err)}`); });
    if (info.status === 401) {
      this.unauthorized();
      return;
    }
    if (!info.ok) throw new Error(`中心的 hub 回答 ${info.status}`);
    this.emit({ type: "up", localPort: port, remotePort: hub.port });

    // Connected: until ssh exits or /v1/health stops answering twice in a row.
    let misses = 0;
    while (!this.closed && ended === undefined && !this.state?.stopped) {
      await sleep(this.opts.watchMs ?? 10_000, wake.signal);
      if (this.closed || ended !== undefined || wake.signal.aborted) break;
      misses = (await healthy(base)) ? 0 : misses + 1;
      if (misses >= 2) throw new Error("中心的 hub 不再应答");
    }
    if (ended !== undefined && !this.closed) throw new Error(ended);
  }
}

async function healthy(base: string): Promise<boolean> {
  try {
    const response = await fetch(`${base}/v1/health`, { signal: AbortSignal.timeout(3_000) });
    await response.body?.cancel();
    return response.ok;
  } catch {
    return false;
  }
}
