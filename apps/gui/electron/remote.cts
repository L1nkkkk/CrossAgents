// Remote mode, pure parts (docs/MULTI-NODE.md §4.3, §14 "SSH 隧道"): checking what the person
// typed, building ssh's arguments, reading what the hub's machine answers, the reconnect delays,
// the tunnel's state and the saved pairing (`<userData>/remote.json`). No Electron here, so the
// tests run under plain node; the process side lives in remoteTunnel.cts.

import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";

/** Where the hub runs and how to reach it over SSH (src/bridge.d.ts `RemoteTarget`). */
export type RemoteTarget = { host: string; user?: string; sshPort?: number; command?: string };
export type TunnelState = { status: "connecting" | "connected" | "offline"; localPort?: number; remotePort?: number; reason?: string; stopped?: boolean };
export type ConnectionState = {
  mode: "local" | "remote";
  remote?: { target: RemoteTarget; deviceName: string; deviceId: string; pairedAt: string };
  tunnel?: TunnelState;
  defaultDeviceName: string;
};
export type PairStep = "locate" | "pair" | "tunnel" | "check";

// ---------------------------------------------------------------------------------------
// Input checks: everything below that reaches ssh or the remote shell has passed these.

const NAME = /^[A-Za-z0-9._-]+$/;
/** A bracketed IPv6 address: hex digits, colons, dots (an embedded IPv4), an optional zone. */
const IPV6 = /^\[[0-9A-Fa-f:.]+(%[A-Za-z0-9._-]+)?\]$/;
/** What a command path on the hub's machine may not contain: shell and cmd.exe specials. */
const UNSAFE_COMMAND = /["$`%!&|;<>^\r\n]/;

export class InputError extends Error {}

export function checkHost(host: unknown): string {
  if (typeof host !== "string" || !host) throw new InputError("请填写主机");
  if (host.startsWith("-")) throw new InputError("主机不能以 - 开头");
  if (!NAME.test(host) && !IPV6.test(host)) throw new InputError("主机只能包含字母、数字、. _ -，IPv6 地址请用方括号");
  return host;
}

export function checkUser(user: unknown): string | undefined {
  if (user === undefined || user === null || user === "") return undefined;
  if (typeof user !== "string" || user.startsWith("-") || !NAME.test(user)) {
    throw new InputError("用户名只能包含字母、数字、. _ -，且不能以 - 开头");
  }
  return user;
}

export function checkPort(port: unknown, what = "SSH 端口"): number | undefined {
  if (port === undefined || port === null) return undefined;
  if (typeof port !== "number" || !Number.isInteger(port) || port < 1 || port > 65535) {
    throw new InputError(`${what}必须是 1–65535 的整数`);
  }
  return port;
}

/** A command path on the hub's machine, or undefined. It is passed in double quotes. */
export function checkCommand(command: unknown): string | undefined {
  if (command === undefined || command === null) return undefined;
  if (typeof command !== "string") throw new InputError("命令必须是文字");
  const trimmed = command.trim();
  if (!trimmed) return undefined;
  if (UNSAFE_COMMAND.test(trimmed)) {
    throw new InputError("命令只能是一个路径，不能包含双引号、$、反引号、%、!、&、|、;、<、>、^ 或换行");
  }
  return trimmed;
}

export function checkDeviceName(name: unknown): string {
  if (typeof name !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(name)) {
    throw new InputError("设备名要 1–64 个字母、数字、. _ -，以字母或数字开头");
  }
  return name;
}

export function checkTarget(target: unknown): RemoteTarget {
  if (!target || typeof target !== "object") throw new InputError("请填写主机");
  const t = target as Record<string, unknown>;
  const out: RemoteTarget = { host: checkHost(t.host) };
  const user = checkUser(t.user);
  if (user) out.user = user;
  const port = checkPort(t.sshPort);
  if (port !== undefined) out.sshPort = port;
  const command = checkCommand(t.command);
  if (command) out.command = command;
  return out;
}

/** `<host name>-gui`, cleaned to what a device name allows. */
export function defaultDeviceName(hostname: string): string {
  let base = hostname.replace(/[^A-Za-z0-9._-]+/g, "-").replace(/^[^A-Za-z0-9]+/, "");
  base = base.slice(0, 64 - "-gui".length).replace(/[^A-Za-z0-9]+$/, "");
  return base ? `${base}-gui` : "crossagents-gui";
}

/** How the target is shown (tray, reasons): `user@host:port`. */
export function describeTarget(target: RemoteTarget): string {
  return `${target.user ? `${target.user}@` : ""}${target.host}${target.sshPort ? `:${target.sshPort}` : ""}`;
}

// ---------------------------------------------------------------------------------------
// ssh's command line. Always an argument array for spawn / execFile, never a local shell.

/** Which ssh to run: Windows' own OpenSSH first (Git's ssh does not use the Windows agent). */
export function sshProgram(platform: NodeJS.Platform, env: NodeJS.ProcessEnv, exists: (path: string) => boolean): string {
  if (platform === "win32") {
    const system = join(env.SystemRoot || env.SYSTEMROOT || "C:\\Windows", "System32", "OpenSSH", "ssh.exe");
    if (exists(system)) return system;
    return "ssh.exe";
  }
  return "ssh";
}

/** Options every ssh call carries: never prompt, give up connecting after 10 s. */
const COMMON = ["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"];

/** The host as ssh takes it: without the brackets of an IPv6 address. */
function sshHost(host: string): string {
  return IPV6.test(host) ? host.slice(1, -1) : host;
}

function destination(target: RemoteTarget): string[] {
  const out: string[] = [];
  if (target.user) out.push("-l", target.user);
  if (target.sshPort) out.push("-p", String(target.sshPort));
  // `--` ends ssh's options: the host can never be read as one.
  out.push("--", sshHost(target.host));
  return out;
}

/** Runs `command` on the hub's machine. `command` is built here from checked values only. */
export function execArgs(target: RemoteTarget, command: string): string[] {
  return [...COMMON, ...destination(target), command];
}

/** The forwarding: `127.0.0.1:<localPort>` here to `127.0.0.1:<remotePort>` there. */
export function tunnelArgs(target: RemoteTarget, localPort: number, remotePort: number): string[] {
  return [
    ...COMMON,
    "-N",
    "-o", "ExitOnForwardFailure=yes",
    "-o", "ServerAliveInterval=15",
    "-o", "ServerAliveCountMax=3",
    "-L", `127.0.0.1:${localPort}:127.0.0.1:${remotePort}`,
    ...destination(target),
  ];
}

/** Where the hub's `hub.json` is: a POSIX shell or PowerShell first, then cmd.exe (Windows' default). */
export const LOCATE_COMMANDS = ["cat ~/.crossagents/hub.json", 'type "%USERPROFILE%\\.crossagents\\hub.json"'];

export const NOT_RUNNING = "中心上的 hub 没在运行（或不在默认位置，可在高级里填命令）";

/** What the hub's machine says about its hub (crates/hub/src/paths.rs `HubFile`). */
export type RemoteHub = { port: number; exe?: string };

export function parseHubFile(text: string): RemoteHub {
  const value = JSON.parse(text.replace(/^\uFEFF/, "").trim()) as { port?: unknown; exe?: unknown };
  const port = checkPort(value?.port, "hub.json 的端口");
  if (port === undefined) throw new Error("hub.json has no port");
  return typeof value.exe === "string" && value.exe ? { port, exe: value.exe } : { port };
}

/**
 * The `crossagents` to run there: the person's > the running hub's own exe > `crossagents` on
 * its PATH. A path is double-quoted; an exe from hub.json that would not pass the person's
 * check is not used.
 */
export function remoteCommand(target: RemoteTarget, hub: RemoteHub | undefined): string {
  if (target.command) return `"${target.command}"`;
  if (hub?.exe) {
    try {
      const exe = checkCommand(hub.exe);
      if (exe) return `"${exe}"`;
    } catch {
      // Odd characters in the path: fall through to PATH.
    }
  }
  return "crossagents";
}

/** Decide whether to probe, then apply PowerShell's call operator to a Windows path. */
export function shellCommand(method: "cat" | "type", command: string, probe: string | null | undefined): { command: string; probe: boolean } {
  const eligible = method === "cat" && /^"(?:[A-Za-z]:[\\/]|\\\\)/.test(command);
  if (!eligible) return { command, probe: false };
  if (probe === undefined) return { command, probe: true };
  return { command: probe !== null && /^\d+$/.test(probe.trim()) ? `& ${command}` : command, probe: false };
}

export function pairCommand(command: string, deviceName: string): string {
  return `${command} device add ${checkDeviceName(deviceName)} --json`;
}

export type Paired = { deviceId: string; deviceName: string; token: string };

/** `device add --json` (crates/cli/src/cmd/device.rs): `{ device, token }`. */
export function parseDeviceAdd(text: string): Paired {
  let value: { device?: { id?: unknown; name?: unknown }; token?: unknown };
  try {
    value = JSON.parse(text.replace(/^\uFEFF/, "").trim());
  } catch {
    throw new Error(`device add 的输出不是 JSON：${text.trim().slice(0, 200)}`);
  }
  const { device, token } = value ?? {};
  if (typeof token !== "string" || !/^[A-Za-z0-9_.-]{8,256}$/.test(token)) throw new Error("device add 的输出里没有 token");
  if (typeof device?.id !== "string" || !device.id) throw new Error("device add 的输出里没有设备 id");
  return { deviceId: device.id, deviceName: typeof device.name === "string" ? device.name : "", token };
}

// ---------------------------------------------------------------------------------------
// Reconnecting.

export const BACKOFF_MS = [1_000, 2_000, 5_000, 10_000, 30_000];

/** The wait before reconnect attempt `attempt` (0-based), capped at 30 s. */
export function backoff(attempt: number): number {
  return BACKOFF_MS[Math.min(Math.max(attempt, 0), BACKOFF_MS.length - 1)];
}

export const REVOKED = "这台设备已被吊销或 token 失效，请重新配对";

export type TunnelEvent =
  | { type: "connecting" }
  | { type: "up"; localPort: number; remotePort: number }
  | { type: "down"; reason: string }
  | { type: "unauthorized" }
  | { type: "stopped"; reason: string };

/**
 * The tunnel's next state. Once stopped (revoked, or the token could not be read) nothing but
 * a new `connecting` (reconnect, a new pairing) moves it on.
 */
export function tunnelReduce(state: TunnelState | undefined, event: TunnelEvent): TunnelState {
  const ports = { localPort: state?.localPort, remotePort: state?.remotePort };
  if (state?.stopped && event.type !== "connecting" && event.type !== "stopped") return state;
  switch (event.type) {
    case "connecting":
      return clean({ status: "connecting", ...ports });
    case "up":
      return { status: "connected", localPort: event.localPort, remotePort: event.remotePort };
    case "down":
      return clean({ status: "offline", ...ports, reason: event.reason });
    case "unauthorized":
      return clean({ status: "offline", ...ports, reason: REVOKED, stopped: true });
    case "stopped":
      return clean({ status: "offline", ...ports, reason: event.reason, stopped: true });
  }
}

function clean<T extends object>(value: T): T {
  for (const key of Object.keys(value) as (keyof T)[]) if (value[key] === undefined) delete value[key];
  return value;
}

// ---------------------------------------------------------------------------------------
// The saved pairing: `<userData>/remote.json`. The token is stored only encrypted.

/** Encrypts and decrypts the token (Electron's safeStorage in the app, a stand-in in tests). */
export type Crypto = {
  /** Why encryption cannot be used here, or undefined when it can. */
  unavailable(): string | undefined;
  encrypt(text: string): Buffer;
  decrypt(data: Buffer): string;
};

export type RemoteRecord = {
  mode: "local" | "remote";
  target: RemoteTarget;
  deviceName: string;
  deviceId: string;
  pairedAt: string;
  localPort?: number;
  /** base64 of the encrypted token. */
  token: string;
};

export const remoteFile = (userData: string) => join(userData, "remote.json");

export async function readRecord(file: string): Promise<RemoteRecord | undefined> {
  let text: string;
  try {
    text = await readFile(file, "utf8");
  } catch {
    return undefined;
  }
  try {
    const value = JSON.parse(text) as Partial<RemoteRecord>;
    if (!value || typeof value.token !== "string" || typeof value.deviceId !== "string") return undefined;
    const target = checkTarget(value.target);
    return {
      mode: value.mode === "remote" ? "remote" : "local",
      target,
      deviceName: typeof value.deviceName === "string" ? value.deviceName : "",
      deviceId: value.deviceId,
      pairedAt: typeof value.pairedAt === "string" ? value.pairedAt : "",
      ...(checkPortQuiet(value.localPort) ? { localPort: value.localPort } : {}),
      token: value.token,
    };
  } catch {
    return undefined;
  }
}

function checkPortQuiet(port: unknown): boolean {
  try {
    return checkPort(port) !== undefined;
  } catch {
    return false;
  }
}

/** Writes via a temporary file, so a crash never leaves half a record. */
export async function writeRecord(file: string, record: RemoteRecord): Promise<void> {
  await mkdir(dirname(file), { recursive: true });
  const partial = `${file}.partial`;
  await writeFile(partial, JSON.stringify(record, null, 2), { mode: 0o600 });
  await rename(partial, file);
}

export async function removeRecord(file: string): Promise<void> {
  await rm(file, { force: true });
}

/** Refuses (throws) rather than ever writing the token in the clear. */
export function sealToken(crypto: Crypto, token: string): string {
  const why = crypto.unavailable();
  if (why) throw new Error(`无法安全保存 token：${why}。没有保存配对。`);
  return crypto.encrypt(token).toString("base64");
}

/** The token, or undefined when it cannot be decrypted (another user, a reset keychain). */
export function openToken(crypto: Crypto, sealed: string): string | undefined {
  try {
    if (crypto.unavailable()) return undefined;
    const token = crypto.decrypt(Buffer.from(sealed, "base64"));
    return token || undefined;
  } catch {
    return undefined;
  }
}

export const REPAIR = "请重新配对";

/** The page's view: never the token. */
export function publicState(record: RemoteRecord | undefined, tunnel: TunnelState | undefined, deviceName: string): ConnectionState {
  const state: ConnectionState = { mode: record?.mode ?? "local", defaultDeviceName: deviceName };
  if (record) {
    state.remote = { target: { ...record.target }, deviceName: record.deviceName, deviceId: record.deviceId, pairedAt: record.pairedAt };
    if (record.mode === "remote" && tunnel) state.tunnel = { ...tunnel };
  }
  return state;
}
