// 内置打洞 on the 连接 page (docs/MULTI-NODE.md §14): what a pasted pairing code says before it
// is used, how the hub's entrance stands, and what a failed pairing means. Pure: the page
// renders these words, the tests pin them. The main process checks the code again
// (electron/link.cts `decodeCode`): this copy only lets the page say what is wrong at once.

import type { LinkStatus } from "@bindings/LinkStatus";
import type { PairStep, TunnelState } from "../bridge";
import type { Tone } from "./connection";

export const CODE_PREFIX = "ca1-";

/** What the page shows under the code box. */
export type CodePreview =
  | { ok: true; host: string; expires: Date; relay: boolean; lan: number }
  | { ok: false; error: string };

function base64url(body: string): string {
  const b64 = body.replace(/-/g, "+").replace(/_/g, "/");
  const padded = b64 + "=".repeat((4 - (b64.length % 4)) % 4);
  const bytes = Uint8Array.from(atob(padded), (c) => c.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

/** `undefined` for an empty box; else what the code says, or what is wrong with it. */
export function previewCode(code: string, now = new Date()): CodePreview | undefined {
  const text = code.replace(/\s+/g, "");
  if (!text) return undefined;
  if (!text.startsWith(CODE_PREFIX)) return { ok: false, error: `这不是配对码：配对码以 ${CODE_PREFIX} 开头。` };
  let value: { e?: unknown; s?: unknown; h?: unknown; x?: unknown; r?: unknown; a?: unknown };
  try {
    value = JSON.parse(base64url(text.slice(CODE_PREFIX.length)));
  } catch {
    return { ok: false, error: "配对码不完整或被改动过：请在中心上重新复制一次。" };
  }
  if (typeof value?.e !== "string" || typeof value.s !== "string" || typeof value.x !== "number") {
    return { ok: false, error: "配对码不完整或被改动过：请在中心上重新复制一次。" };
  }
  const expires = new Date(value.x * 1000);
  if (expires.getTime() <= now.getTime()) return { ok: false, error: "这个配对码已经过期：在中心上重新生成一个（30 分钟内有效）。" };
  return {
    ok: true,
    host: typeof value.h === "string" && value.h ? value.h : "中心",
    expires,
    relay: typeof value.r === "string" && !!value.r,
    lan: Array.isArray(value.a) ? value.a.length : 0,
  };
}

/** "29:05": minutes and seconds left, or "" once it is over. */
export function countdown(until: Date, now = new Date()): string {
  const left = Math.floor((until.getTime() - now.getTime()) / 1000);
  if (left <= 0) return "";
  return `${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}`;
}

/** Pairing by code: its three steps, in order, as the person reads them. */
export const CODE_STEPS: readonly { key: PairStep; label: string }[] = [
  { key: "redeem", label: "打洞连到中心，换取设备 token" },
  { key: "forward", label: "在本机起转发" },
  { key: "check", label: "检查设备 token" },
];

export function codeStepIndex(step: PairStep | undefined): number {
  return step ? CODE_STEPS.findIndex((s) => s.key === step) : -1;
}

/** How the hub's entrance stands, for the 中心 side of the page. */
export type EntranceWords = { word: string; tone: Tone; detail: string };

export function entranceWords(status: LinkStatus | undefined): EntranceWords {
  if (!status) return { word: "读取中", tone: "muted", detail: "" };
  switch (status.state) {
    case "off":
      return { word: "未开启", tone: "muted", detail: "开启后，这台电脑可以被别的电脑用配对码连进来；不开端口，也不用 SSH。" };
    case "starting":
      return {
        word: "连接中继中",
        tone: "warn",
        detail: status.proxy
          ? `正在经代理 ${status.proxy} 连接中继。一直停在这里，多半是代理不通或中继被墙，可以换代理或填自建中继。`
          : "正在连接中继。国内网络连 n0 的公共中继常常要代理：hub 用系统代理或 HTTPS_PROXY；一直停在这里，就开系统代理后点「重新开启」，或填自建中继。",
      };
    case "online":
      return {
        word: "在线",
        tone: "ok",
        detail: `别的电脑现在可以用配对码连进来。中继：${status.custom_relay ? "自建 " : ""}${hostOf(status.relay)}${status.proxy ? ` · 经代理 ${status.proxy}` : ""}`,
      };
    case "failed":
      return { word: "没有开起来", tone: "danger", detail: status.error ?? "内置打洞没有开起来。" };
  }
}

function hostOf(url: string | undefined): string {
  if (!url) return "";
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

/** 直连 / 经中继, for a hole-punched tunnel. */
export function pathWords(tunnel: TunnelState | undefined): { word: string; title: string } | undefined {
  if (tunnel?.status !== "connected" || !tunnel.path) return undefined;
  return tunnel.path === "direct"
    ? { word: "直连", title: "打洞成功：数据直接在两台电脑之间走，不经过中继" }
    : { word: "经中继", title: "还没打通，数据经中继转发：能用，延迟会高一些；打通后会自动换成直连" };
}

/** Why pairing by code failed, as far as the person can act on it. */
export type CodeFailure = { title: string; next: string };

export function codeFailure(reason: string): CodeFailure {
  const text = reason.toLowerCase();
  if (/过期|已经用过|expired|重启过/.test(reason)) {
    return { title: "配对码不能用了", next: "配对码只能用一次、30 分钟内有效，中心的 hub 重启后也会作废。在中心上重新生成一个，再粘贴过来。" };
  }
  if (/不是 crossagents 的配对码|不完整|被改动|请粘贴/i.test(reason)) {
    return { title: "配对码不对", next: "在中心上点「复制」，把整段配对码（以 ca1- 开头）原样粘贴过来。" };
  }
  if (/没有打洞程序|找不到 crossagents-link/.test(reason)) {
    return { title: "这个安装包缺少打洞程序", next: "用含 crossagents-link 的安装包重新安装；Mac 自己打包时先 cargo build --release -p crossagents-link（见 docs/INSTALL-MAC.md）。也可以先用下面的 SSH 方式连。" };
  }
  if (/无法安全保存 token/.test(reason)) {
    return { title: "这台电脑不能安全保存 token", next: "系统的加密存储不可用，所以没有配对。在有钥匙串（或 Windows 凭据保护）的账户下再试。" };
  }
  if (/找不到中心|no addressing|连不上中心|timed out|超时|unreachable|中继/i.test(text)) {
    return {
      title: "连不上中心",
      next: "确认中心开着、它的 设置 →「连接」里内置打洞显示「在线」。国内网络两边常常都要代理才能连上中继：开系统代理后重试；两台在同一局域网时不经中继也能连。",
    };
  }
  return { title: "配对没有完成", next: "按上面的原因检查；也可以在中心上重新生成配对码再试一次。" };
}
