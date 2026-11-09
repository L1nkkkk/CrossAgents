// 设置 → 连接 → Tailscale: how peers are ordered and which address "用这个地址" fills in.
// Pure, tested in tailscale.test.ts.

import type { TailscalePeer, TailscaleStatus } from "../bridge";

/** Online peers first, each group by host name; the order is stable between refreshes. */
export function sortPeers(peers: readonly TailscalePeer[]): TailscalePeer[] {
  return [...peers].sort((a, b) => Number(b.online) - Number(a.online) || a.hostName.localeCompare(b.hostName));
}

/** What goes in the host field: the MagicDNS name, else the first IP; undefined when it has neither. */
export const peerAddress = (peer: TailscalePeer): string | undefined => peer.dnsName?.trim() || peer.ips[0];

/** Why the Tailscale section has no peer list, in words; undefined while it is running. */
export function tailscaleNote(status: TailscaleStatus): string | undefined {
  const why = status.error ? `（${status.error}）` : "";
  switch (status.state) {
    case "missing":
      return "没找到 tailscale 命令。两台机器都要装 Tailscale（tailscale.com/download）并登录同一个账号，装好后点「刷新」。";
    case "needs-login":
      return `Tailscale 已安装但还没登录：打开 Tailscale 登录，要和另一台机器用同一个账号。${why}`;
    case "stopped":
      return `Tailscale 没有在运行，或读不到它的状态：先启动 Tailscale。${why}`;
    case "running":
      return undefined;
  }
}
