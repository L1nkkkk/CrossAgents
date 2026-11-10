// The extension management page's judgments, kept apart from its markup so they can be tested
// without a browser (docs/EXTENSIONS.md §6, §7): what state an extension is in, which version
// the approval screen will approve, and why a switch cannot be moved.
//
// The rule the approval screen exists for: what the person sees is exactly what is submitted.
// Nothing here enables a version the page has not shown (`approveFingerprint`), and a pending
// record that moved while the screen was open reads as stale instead of quietly switching.

import type { ExtensionAccess } from "@bindings/ExtensionAccess";
import type { ExtensionDiffResponse } from "@bindings/ExtensionDiffResponse";
import type { ExtensionFileChange } from "@bindings/ExtensionFileChange";
import type { ExtensionInfo } from "@bindings/ExtensionInfo";
import type { ExtensionServiceState } from "@bindings/ExtensionServiceState";
import type { ExtensionVersion } from "@bindings/ExtensionVersion";

const serviceStateLabels: Record<ExtensionServiceState, string> = {
  stopped: "已停止",
  starting: "正在启动",
  running: "运行中",
  backoff: "等待重启",
};

export function serviceStateText(state: ExtensionServiceState): string {
  return serviceStateLabels[state];
}

/** How the page itself sees an extension: the host's status, not the hub's. */
export type PageState = { status?: "loading" | "ready" | "failed" | "disabled"; error?: string };

/** What a status chip is coloured by, in the page's own vocabulary. */
export type Tone = "ok" | "accent" | "warn" | "danger" | "off";

export type Badge = { key: string; label: string; tone: Tone; title: string };

/** `3f9a1c02…`: enough of a fingerprint to tell two versions apart on screen. */
export function shortFingerprint(fingerprint: string, length = 8): string {
  return fingerprint.length <= length ? fingerprint : `${fingerprint.slice(0, length)}…`;
}

/** Bytes as a person reads them. */
export function bytesText(bytes: number): string {
  return bytes < 1024 ? `${bytes} B` : bytes < 1024 * 1024 ? `${(bytes / 1024).toFixed(1)} KB` : `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** Files and bytes together, for a whole snapshot. */
export function sizeText(files: number, bytes: number): string {
  return `${files} 个文件 · ${bytesText(bytes)}`;
}

/** When a snapshot was taken, in the page's local time. */
export function takenText(iso: string): string {
  const at = new Date(iso);
  return Number.isNaN(at.getTime()) ? iso : at.toLocaleString("zh-CN", { hour12: false });
}

/** Every state an extension can be in, as chips; an extension can carry several at once. */
export function badges(info: ExtensionInfo, ctx: { safeMode: boolean; page?: PageState }): Badge[] {
  const out: Badge[] = [];
  const pending = info.pending;
  if (ctx.safeMode && (info.enabled || pending)) {
    out.push({
      key: "safe",
      label: "安全模式下未加载",
      tone: "warn",
      title: "安全模式里不加载任何扩展：它的面板、页面、卡片和设置都不在。原因和退出办法在这一页顶部的说明里。",
    });
  }
  if (info.error) {
    out.push({ key: "failed", label: "失败", tone: "danger", title: `源目录有问题：${explainedText(info.error)}` });
  } else if (ctx.page?.status === "failed") {
    out.push({ key: "load-failed", label: "加载失败", tone: "danger", title: ctx.page.error ? explainedText(ctx.page.error) : "页面没能加载这个扩展的入口" });
  }
  if (pending && info.enabled) {
    out.push({
      key: "update",
      label: "有更新",
      tone: "accent",
      title: `源目录变过：${shortFingerprint(pending.version.fingerprint)} 这一版在等着批准，现在跑的还是${info.current ? ` ${shortFingerprint(info.current.fingerprint)}` : "旧版本"}`,
    });
  }
  if (pending && !info.enabled) {
    out.push({ key: "pending", label: "待批准", tone: "warn", title: "还没有人批准过它：批准之前它一行代码都不会跑。" });
  }
  if (info.auto_trust) {
    out.push({ key: "auto", label: "自动信任更新", tone: "accent", title: "源目录一变，hub 就自己生成新快照并切换，不再逐次让人确认。" });
  }
  out.push(info.enabled ? { key: "enabled", label: "已启用", tone: "ok", title: "调用和界面都用它当前批准的版本。" } : { key: "off", label: "未启用", tone: "off", title: "没有批准过，或者被停用了。" });
  return out;
}

/** What the person is about to approve: the version this screen shows, and nothing newer. */
export type ApprovalView =
  | { kind: "none" }
  | { kind: "ready"; version: ExtensionVersion }
  | { kind: "stale"; version: ExtensionVersion; seen: string }
  | { kind: "gone"; seen: string };

/**
 * `seen` is the fingerprint the screen opened with. A pending record that has since been
 * replaced reads as `stale`, one that was approved or withdrawn as `gone` — both say so, and
 * neither offers the button (the hub would answer 409 anyway; the point is not to try).
 */
export function approvalView(info: ExtensionInfo, seen?: string): ApprovalView {
  const pending = info.pending;
  if (!pending) return seen ? { kind: "gone", seen } : { kind: "none" };
  if (seen && seen !== pending.version.fingerprint) return { kind: "stale", version: pending.version, seen };
  return { kind: "ready", version: pending.version };
}

/** The exact fingerprint the 批准并启用 button submits; undefined when there is nothing to approve. */
export function approveFingerprint(view: ApprovalView): string | undefined {
  return view.kind === "ready" ? view.version.fingerprint : undefined;
}

/**
 * Which fingerprint the screen should hold on to. It keeps what it already has (so a version
 * that moved still reads as `stale`), takes the waiting one when it has looked at none yet, and
 * ignores a version this window just approved (`approved`): otherwise the version that arrives
 * after an approval would read as "the one you saw is gone", when the person had approved it.
 */
export function seenAfter(seen: string | undefined, approved: string | undefined, info: ExtensionInfo): string | undefined {
  const pending = info.pending?.version.fingerprint;
  if (!pending || pending === approved) return seen;
  return seen ?? pending;
}

/** Why the approve button cannot be used, or undefined when it can. */
export function approveBlockedReason(info: ExtensionInfo, view: ApprovalView): string | undefined {
  switch (view.kind) {
    case "none":
      return info.enabled ? "没有等待批准的版本：源目录和正在运行的版本一样。" : "还没有可批准的版本：等 hub 扫到源目录里的改动。";
    case "stale":
      return "这一版已经变了：先看新的一版，再决定要不要启用。";
    case "gone":
      return "那条待批准的记录已经不在了（被新的一版顶掉，或者已经批准过）。";
    case "ready":
      return info.error ? `源目录有问题，hub 没法为它做快照：${explainedText(info.error)}` : undefined;
  }
}

/**
 * Turning a stopped extension back on. This only ever re-enables a version already approved —
 * `info.current.fingerprint` — even while another version waits for approval: the hub does the
 * same (a fingerprint equal to `current` is re-enabled and leaves the pending record alone), and
 * a person who stopped an extension should not have to approve code they have not read to get
 * the old one back. A waiting version still only runs through the approval card.
 */
export type EnableState = { fingerprint: string; title: string } | { reason: string };

export function enableState(info: ExtensionInfo): EnableState {
  if (info.enabled) return { reason: "这个扩展已经启用了。" };
  if (info.current) {
    const fingerprint = info.current.fingerprint;
    return {
      fingerprint,
      title: `重新启用已批准的 ${shortFingerprint(fingerprint)}（不换版本）${info.pending ? "；待批准的那一版不会因此启用" : ""}`,
    };
  }
  if (info.pending) return { reason: "还没有批准过任何版本：先把下面待批准的一版看过再批准。" };
  return { reason: "还没有批准过任何版本：等 hub 扫到源目录里的改动，再批准那一版。" };
}

/** Why 停用 cannot be used, or undefined when it can. */
export function disableBlockedReason(info: ExtensionInfo): string | undefined {
  return info.enabled ? undefined : "这个扩展现在是停用的。";
}

/**
 * What the auto-trust switch means, kept next to the switch itself. It also accepts a pending
 * version on the way in (`Registry::set_auto_trust`), which must not be a surprise.
 */
export const AUTO_TRUST_HINT =
  "打开后，这个扩展的源目录一变，hub 就自己生成新快照并切换，不再逐次确认；打开时如果正有等待批准的版本，会一并批准并启用它。";

export function autoTrustBlockedReason(info: ExtensionInfo): string | undefined {
  return info.error ? `源目录现在有问题，hub 做不出快照，改好之后才能打开自动信任：${explainedText(info.error)}` : undefined;
}

/** What a version's `access` lets it do, in the page's words. */
export function accessText(access: ExtensionAccess): { label: string; detail: string } {
  return access === "write"
    ? { label: "可写", detail: "它调 hub 时能读也能改：可以替你做写操作（会话、任务、笔记等）。" }
    : { label: "只读", detail: "它调 hub 时只能读，改不了任何东西。" };
}

/** An error as the hub or a process wrote it, with a Chinese lead when the original is English.
 *  `raw` is the original, kept for searching and reporting; absent when `text` already is it. */
export type ExplainedError = { text: string; raw?: string };

const ERROR_LEADS: [RegExp, (m: RegExpMatchArray) => string][] = [
  [/exceeds\s+(\d+(?:\.\d+)?)\s*([KMG]B)/i, (m) => `扩展目录超过 ${m[1]} ${m[2].toUpperCase()} 的上限，hub 不给它做快照`],
  [/exit(?:ed)?\s+(?:with\s+)?(?:code|status)\s+(-?\d+)/i, (m) => `进程退出了，退出码 ${m[1]}`],
  [/signal\s*:?\s*(\w+)/i, (m) => `进程被信号 ${m[1]} 结束`],
  [/\b(?:toml|parse|invalid|expected|unknown field|missing field)\b/i, () => "extension.toml 或清单写得不对"],
  [/\b(?:not found|no such file|enoent|cannot find)\b/i, () => "找不到要用的文件或程序"],
  [/\b(?:permission denied|eacces|eperm|access is denied)\b/i, () => "没有权限读写或运行"],
  [/\btimed?\s*out\b/i, () => "超时了"],
  [/\b(?:econnrefused|connection refused)\b/i, () => "连接被拒绝"],
];

export function explainError(message: string): ExplainedError {
  const raw = message.trim();
  // Already in Chinese (the hub's own words): show as it is.
  if (!raw || /[一-鿿]/.test(raw)) return { text: raw };
  for (const [pattern, lead] of ERROR_LEADS) {
    const m = raw.match(pattern);
    if (m) return { text: lead(m), raw };
  }
  return { text: "出错了，原文如下", raw };
}

/** The same, on one line: 中文说明（原文：…）. */
export function explainedText(message: string): string {
  const e = explainError(message);
  return e.raw ? `${e.text}（原文：${e.raw}）` : e.text;
}

/** One version's facts, as the approval screen and the running-version card show them. */
export type VersionFacts = {
  fingerprint: string;
  short: string;
  api: number;
  access: ExtensionAccess;
  tools: number;
  prompts: number;
  guiEntry?: string;
  files: string;
  takenAt: string;
};

export function versionFacts(version: ExtensionVersion): VersionFacts {
  return {
    fingerprint: version.fingerprint,
    short: shortFingerprint(version.fingerprint),
    api: version.api,
    access: version.access,
    tools: version.tools.length,
    prompts: version.prompts.length,
    guiEntry: version.gui_entry,
    files: sizeText(version.files, version.bytes),
    takenAt: takenText(version.taken_at),
  };
}

/** `+3 −1`, or `二进制` for a file the hub does not hand out as text. */
export function changeCount(f: ExtensionFileChange): string {
  if (f.binary) return "二进制";
  const parts = [];
  if (f.added) parts.push(`+${f.added}`);
  if (f.removed) parts.push(`−${f.removed}`);
  return parts.length ? parts.join(" ") : "无文本改动";
}

export function changeLabel(change: ExtensionFileChange["change"]): string {
  return change === "added" ? "新增" : change === "removed" ? "删除" : "修改";
}

/** What the diff section is against. */
export function diffLead(diff: ExtensionDiffResponse): string {
  return diff.against ? `对正在运行的 ${shortFingerprint(diff.against)}` : "第一个版本：下面全是新增的文件";
}

/** The request the 让 agent 做一个插件 button leaves in a new conversation's composer. */
export function pluginRequestDraft(want: string): { title: string; text: string } {
  const ask = want.trim();
  return {
    title: ask ? `做一个插件：${ask.slice(0, 24)}` : "做一个插件",
    text: [
      ask ? `给我做一个扩展（插件）：${ask}` : "给我做一个扩展（插件），具体做什么先问我。",
      "",
      "按 docs/EXTENSIONS.md 来：",
      "1. 先调 extension_api，看接口版本、事件、group 和 GUI 插槽；",
      "2. 写在 ~/.crossagents/extensions/<名字>/ 下：extension.toml，工具脚本，要面板就加 gui/index.js；",
      "3. 写完用 extension_check 静态校验，再用 extension_request_enable 建一条待批准记录。",
      "不要自己想办法启用它：启用和自动信任只有我能点，我会在「扩展」页里看这一版的工具、说明、面板和权限（access），再决定。",
    ].join("\n"),
  };
}

/** The groups a tool declares, or why nobody can call it. */
export function groupsText(groups: readonly string[]): string {
  return groups.length ? groups.join("、") : "没有 group：谁都调不了";
}
