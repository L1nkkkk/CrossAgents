// Hole punching, the GUI's side (docs/MULTI-NODE.md §14「内置打洞」): reading a pairing code,
// running `crossagents-link redeem` to turn it into a device token, and the arguments of
// `crossagents-link connect`, the local forward the tunnel keeps up. Secrets go to the program
// on stdin, never on its command line. The tunnel loop itself is remoteTunnel.cts's.

import { execFile, spawn, type ChildProcess } from "node:child_process";
import { checkDeviceName, checkLinkTarget, hostLabel, InputError, type LinkTarget, type Paired } from "./remote.cjs";

/** The crossagents-link to run: `program` with `prefix` before its own arguments (tests: node + a script). */
export type LinkProgram = {
  program: string;
  prefix: string[];
  /** The environment to start it with: the system proxy added (main.cts), or the tests' own. */
  env(): Promise<NodeJS.ProcessEnv>;
};

export const CODE_PREFIX = "ca1-";

/** What a pairing code says, as far as this side needs before redeeming it. */
export type CodeInfo = { link: LinkTarget; host: string; expires: Date };

/** Reads a pasted code (crates/link/src/lib.rs `Invite`); throws InputError with what is wrong. */
export function decodeCode(code: unknown, now = new Date()): CodeInfo {
  const text = typeof code === "string" ? code.replace(/\s+/g, "") : "";
  if (!text) throw new InputError("请粘贴配对码");
  if (!text.startsWith(CODE_PREFIX)) throw new InputError(`这不是 CrossAgents 的配对码（配对码以 ${CODE_PREFIX} 开头）`);
  const body = text.slice(CODE_PREFIX.length);
  if (!/^[A-Za-z0-9_-]+$/.test(body) || body.length > 4096) throw new InputError("配对码不完整或被改动过：请从中心重新复制一次");
  let value: Record<string, unknown>;
  try {
    value = JSON.parse(Buffer.from(body, "base64url").toString("utf8"));
  } catch {
    throw new InputError("配对码不完整或被改动过：请从中心重新复制一次");
  }
  if (!value || typeof value !== "object" || typeof value.s !== "string" || typeof value.x !== "number") {
    throw new InputError("配对码不完整或被改动过：请从中心重新复制一次");
  }
  const link = checkLinkTarget({ endpoint: value.e, relay: value.r, customRelay: value.c === true, addrs: value.a });
  const expires = new Date(value.x * 1000);
  if (expires.getTime() <= now.getTime()) throw new InputError("这个配对码已经过期：在中心上重新生成一个（30 分钟内有效）");
  return { link, host: hostLabel(value.h), expires };
}

/** `crossagents-link redeem`'s arguments; the code goes on stdin. */
export function redeemArgs(deviceName: string, os: string): string[] {
  return ["redeem", "--name", checkDeviceName(deviceName), "--os", os];
}

/** `crossagents-link connect`'s arguments; the token goes on stdin. Values were checked by checkLinkTarget. */
export function connectArgs(target: LinkTarget, localPort: number): string[] {
  const link = checkLinkTarget(target);
  const args = ["connect", "--endpoint", link.endpoint, "--listen", `127.0.0.1:${localPort}`];
  if (link.relay) args.push("--relay", link.relay);
  if (link.relay && link.customRelay) args.push("--custom-relay");
  for (const addr of link.addrs ?? []) args.push("--addr", addr);
  return args;
}

/** What `redeem` printed: the device and its token, plus where the hub is. */
export function parseRedeem(text: string): Paired {
  let value: { device?: { id?: unknown; name?: unknown }; token?: unknown };
  try {
    value = JSON.parse(text.trim());
  } catch {
    throw new Error("crossagents-link 的输出不是 JSON");
  }
  const { device, token } = value ?? {};
  if (typeof token !== "string" || !/^[A-Za-z0-9_.-]{8,256}$/.test(token)) throw new Error("中心没有给出设备 token");
  if (typeof device?.id !== "string" || !/^[A-Za-z0-9._-]{1,64}$/.test(device.id)) throw new Error("中心给出的设备 id 无效");
  let deviceName: string;
  try { deviceName = checkDeviceName(device.name); } catch { throw new Error("中心给出的设备名无效"); }
  return { deviceId: device.id, deviceName, token };
}

/**
 * The proxy Chromium would use for the relay (`session.resolveProxy`: "PROXY h:p; DIRECT"),
 * as an \`HTTPS_PROXY\` value; undefined for DIRECT or a SOCKS-only answer.
 */
export function proxyFromResolve(answer: string | undefined): string | undefined {
  for (const part of (answer ?? "").split(";")) {
    const [kind, host] = part.trim().split(/\s+/, 2);
    if (!host) continue;
    if (/^(PROXY|HTTP)$/i.test(kind)) return `http://${host}`;
    if (/^HTTPS$/i.test(kind)) return `https://${host}`;
  }
  return undefined;
}

/** Whether the environment already names a proxy (then it is left alone). */
export function hasProxy(env: NodeJS.ProcessEnv): boolean {
  return ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy", "HTTP_PROXY", "http_proxy"].some((k) => !!env[k]?.trim());
}

/** Exit code of `connect` when the hub refused the token (crates/link/src/main.rs). */
export const EXIT_UNAUTHORIZED = 3;

const REDEEM_TIMEOUT_MS = 90_000;

/** Runs `redeem`: hole-punches to the hub and trades the code for a device token. */
export async function redeemCode(program: LinkProgram, code: string, deviceName: string, os: string): Promise<{ paired: Paired; info: CodeInfo }> {
  const info = decodeCode(code);
  const env = await program.env();
  const stdout = await new Promise<string>((done, fail) => {
    const child = execFile(
      program.program,
      [...program.prefix, ...redeemArgs(deviceName, os)],
      { timeout: REDEEM_TIMEOUT_MS, windowsHide: true, maxBuffer: 1 << 20, env },
      (err, out, stderr) => {
        if (!err) return done(String(out));
        const e = err as NodeJS.ErrnoException & { killed?: boolean };
        if (e.code === "ENOENT") return fail(new Error(`找不到 crossagents-link（${program.program}）：这个安装包里缺少打洞程序`));
        if (e.killed) return fail(new Error(`连不上中心（${REDEEM_TIMEOUT_MS / 1000} 秒内没有打通）`));
        fail(new Error(linkError(String(stderr ?? "")) || `crossagents-link 失败（退出码 ${String(e.code)}）`));
      },
    );
    child.stdin?.end(`${code.replace(/\s+/g, "")}\n`);
  });
  return { paired: parseRedeem(stdout), info };
}

/** The last error line crossagents-link printed (`Error: …`), without the log noise. */
export function linkError(stderr: string): string {
  const lines = stderr.split(/\r?\n/).map((l) => l.trim()).filter(Boolean);
  const error = lines.findIndex((l) => l.startsWith("Error:"));
  if (error >= 0) return lines.slice(error).join("\n").replace(/^Error:\s*/, "");
  return lines.at(-1) ?? "";
}

/** Starts `connect`; the caller reads its JSON lines and waits for the hub through the port. */
export async function spawnConnect(program: LinkProgram, target: LinkTarget, localPort: number, token: string): Promise<ChildProcess> {
  const env = await program.env();
  const child = spawn(program.program, [...program.prefix, ...connectArgs(target, localPort)], {
    windowsHide: true,
    stdio: ["pipe", "pipe", "pipe"],
    env,
  });
  child.stdin?.on("error", () => {});
  child.stdin?.end(`${token}\n`);
  return child;
}

/** One line `connect` printed (crates/link/src/lib.rs `Event`). */
export type LinkEvent =
  | { event: "listening"; port: number }
  | { event: "connected"; direct: boolean }
  | { event: "path"; direct: boolean }
  | { event: "unreachable"; error: string }
  | { event: "unauthorized"; error: string };

export function parseEvent(line: string): LinkEvent | undefined {
  try {
    const value = JSON.parse(line) as LinkEvent;
    return value && typeof value === "object" && typeof value.event === "string" ? value : undefined;
  } catch {
    return undefined;
  }
}

/** Words for why the hub cannot be reached, from what crossagents-link said. */
export function unreachableWords(error: string): string {
  if (/no addressing information|no relay|relay/i.test(error)) {
    return `找不到中心：中心不在线，或两边都连不上中继（国内网络常要代理，见设置 →「连接」里中心那一栏的说明）。${error ? `\n${error}` : ""}`;
  }
  if (/timed out/i.test(error)) return `连中心超时：中心可能没开，或网络把 UDP 拦了而中继也连不上。${error ? `\n${error}` : ""}`;
  return error ? `连不上中心：${error}` : "连不上中心";
}
