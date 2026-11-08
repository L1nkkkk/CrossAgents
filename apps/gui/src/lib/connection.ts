// 设置 → 连接 (docs/MULTI-NODE.md §4.3, §14): what the window talks to, how the tunnel stands,
// and what a failed pairing means. Pure: the page renders these words, the tests pin them.

import type { ConnectionState, PairStep, RemoteTarget, TunnelState } from "../bridge";

/** A colour a page gives a state; the sidebar's dot and the page's chips share it. */
export type Tone = "ok" | "warn" | "danger" | "muted";

/** The pairing's four steps, in order, as the person reads them. */
export const PAIR_STEPS: readonly { key: PairStep; label: string }[] = [
  { key: "locate", label: "定位中心的 hub" },
  { key: "pair", label: "在中心上签发设备" },
  { key: "tunnel", label: "建立 SSH 隧道" },
  { key: "check", label: "检查设备 token" },
];

/** Where a pairing step stands: `undefined` before the first one. */
export function stepIndex(step: PairStep | undefined): number {
  return step ? PAIR_STEPS.findIndex((s) => s.key === step) : -1;
}

/** "第 2 步，共 4 步：在中心上签发设备", or "" before pairing starts. */
export function stepWords(step: PairStep | undefined): string {
  const i = stepIndex(step);
  if (i < 0) return "";
  return `第 ${i + 1} 步，共 ${PAIR_STEPS.length} 步：${PAIR_STEPS[i].label}`;
}

/** `user@host` as ssh takes it, without the port (ssh wants `-p` for that, not `host:port`). */
function destination(target: RemoteTarget): string {
  return `${target.user ? `${target.user}@` : ""}${target.host}`;
}

/** A display label for a target: `user@host`, with `:port` when it is not the default one. */
export function targetWords(target: RemoteTarget): string {
  const port = target.sshPort && target.sshPort !== 22 ? `:${target.sshPort}` : "";
  return `${destination(target)}${port}`;
}

/** The tunnel's word, its reason or ports, and the colour the page gives it. */
export type StatusWords = { word: string; detail?: string; tone: Tone };

const ports = (tunnel: TunnelState) =>
  tunnel.localPort !== undefined && tunnel.remotePort !== undefined ? `本机 ${tunnel.localPort} → 中心 ${tunnel.remotePort}` : undefined;

/** 连接中 / 已连接 / 未连接：原因, with the forwards it uses. */
export function tunnelWords(tunnel: TunnelState | undefined): StatusWords {
  if (!tunnel) return { word: "未连接", detail: "还没有隧道", tone: "muted" };
  switch (tunnel.status) {
    case "connecting":
      return { word: "连接中", detail: ports(tunnel), tone: "warn" };
    case "connected":
      return { word: "已连接", detail: ports(tunnel), tone: "ok" };
    case "offline": {
      const parts = [tunnel.reason ?? "隧道断开了", ...(tunnel.stopped ? ["不会再自动重连"] : [])];
      return { word: "未连接", detail: parts.join("；"), tone: "danger" };
    }
  }
}

/** What this window talks to: 本机, or 远程 user@host with the tunnel's state. */
export type ConnectionWords = { mode: "local" | "remote"; label: string; status: string; detail?: string; tone: Tone };

export function connectionWords(state: ConnectionState | undefined): ConnectionWords {
  if (state?.mode === "local" && state.bundledHub === false) return { mode: "local", label: "本机", status: "不带 hub", tone: "danger" };
  if (!state || state.mode === "local") return { mode: "local", label: "本机", status: "本机的 hub", tone: "ok" };
  const tunnel = tunnelWords(state.tunnel);
  return {
    mode: "remote",
    label: state.remote ? `远程 ${targetWords(state.remote.target)}` : "远程中心",
    status: tunnel.word,
    detail: tunnel.detail,
    // A saved pairing with no tunnel yet is not connected either.
    tone: tunnel.tone === "muted" ? "danger" : tunnel.tone,
  };
}

/** The sidebar's one-line mark: 本机, or 远程 host and whether it is up. */
export function markWords(state: ConnectionState | undefined): { text: string; tone: Tone; title: string } {
  const words = connectionWords(state);
  if (words.mode === "local") {
    if (state?.bundledHub === false) return { text: "本机 · 不带 hub", tone: "danger", title: "这个安装包不带 hub：在 设置 →「连接」 里连另一台电脑上的 hub" };
    return { text: "本机", tone: "ok", title: "这个窗口连的是本机的 hub；在设置 →「连接」里可以连到远处的中心" };
  }
  const text = words.status === "已连接" ? words.label : `${words.label} · ${words.status}`;
  // 未连接 is red even when there is no tunnel yet (a pairing saved, not started).
  const tone = words.status === "未连接" ? "danger" : words.tone;
  return { text, tone, title: [words.label, words.status, words.detail].filter(Boolean).join(" · ") };
}

/** Why pairing failed, as far as the person can act on it. */
export type PairFailureKind = "host-key" | "auth" | "no-command" | "hub-down" | "ssh-missing" | "network" | "timeout" | "other";
export type PairFailure = { kind: PairFailureKind; title: string; next: string };

/** The command a person runs once in a terminal: ssh's own `-p` for a non-default port. */
const sshOnce = (target?: RemoteTarget) =>
  target ? `ssh ${target.sshPort && target.sshPort !== 22 ? `-p ${target.sshPort} ` : ""}${destination(target)}` : "ssh user@host";

/**
 * Sorts ssh's own words into the one thing to try next (docs/MULTI-NODE.md §14): accept the
 * host key first, make sure the account works, put `crossagents` where the SSH session finds
 * it, or start the hub on the centre.
 */
export function pairFailure(reason: string, target?: RemoteTarget): PairFailure {
  const text = reason.toLowerCase();
  const login = sshOnce(target);
  if (/中心上的 hub 没在运行|hub 没在运行|hub\.json/.test(reason)) {
    return {
      kind: "hub-down",
      title: "中心上的 hub 没在运行",
      next: "先在中心启动 hub（crossagents hub start），再回来重试。",
    };
  }
  if (/command not found|not recognized as an internal or external command|not recognized as the name of a cmdlet|unknown command: crossagents|crossagents: (?:command )?not found|找不到 crossagents|no such file or directory: crossagents/.test(text)) {
    return {
      kind: "no-command",
      title: "中心上找不到 crossagents",
      next: "在中心上把它加到 PATH（或放到默认位置），重启 sshd 让新会话看到它；也可以在「高级」里填 crossagents 的完整路径（docs/MULTI-NODE.md §14）。",
    };
  }
  if (/host key verification failed|remote host identification has changed|no matching host key|authenticity of host/.test(text)) {
    return {
      kind: "host-key",
      title: "SSH 还没信任中心的这台机器",
      next: `先在终端里运行一次 ${login}，看清指纹后回答 yes；连上以后回到这里重试。`,
    };
  }
  if (/permission denied|publickey|password|passphrase|too many authentication failures/.test(text)) {
    return {
      kind: "auth",
      title: "中心不接受这个身份",
      next: `先在终端里运行一次 ${login}，确认密钥或口令可用（密钥在 ~/.ssh/config 与 ssh-agent 里）；再用上面的「配对并连接」重试。`,
    };
  }
  if (/找不到 ssh|enoent/.test(text)) {
    return {
      kind: "ssh-missing",
      title: "这台电脑上找不到 ssh",
      next: "安装系统的 OpenSSH 客户端（Windows 上是「可选功能 → OpenSSH 客户端」），再重试。",
    };
  }
  if (/could not resolve hostname|connection refused|network is unreachable|no route to host|connection reset/.test(text)) {
    return {
      kind: "network",
      title: "连不上中心",
      next: `确认中心的地址和 SSH 端口对：先在终端里运行一次 ${login}，看能不能登进去。`,
    };
  }
  if (/超时|timed out|timeout/.test(text)) {
    return {
      kind: "timeout",
      title: "连接超时",
      next: `确认中心开着、网络通：先在终端里运行一次 ${login}，看它卡在哪里。`,
    };
  }
  return {
    kind: "other",
    title: "配对没有完成",
    next: `按上面的原因检查；也可以先在终端里运行一次 ${login}，看 ssh 自己怎么说。`,
  };
}
