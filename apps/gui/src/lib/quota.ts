// Subscription quota as people read it: window names, reset times, warning levels. Pure and
// vendor-neutral (any provider's AccountStatus), tested in quota.test.ts.

import type { EgressFingerprint } from "@bindings/EgressFingerprint";
import type { RateWindow } from "@bindings/RateWindow";
import { when } from "./time.ts";

const HOUR = 3600;
const DAY = 24 * HOUR;

/** "5 小时限额", "每周限额", "30 天限额"; the vendor's own name when the length is unknown. */
export function windowLabel(w: Pick<RateWindow, "name" | "window_seconds">): string {
  const s = w.window_seconds;
  if (!s) return w.name === "primary" ? "主限额" : w.name === "secondary" ? "次限额" : w.name;
  if (s === 7 * DAY) return "每周限额";
  if (s % DAY === 0) return `${s / DAY} 天限额`;
  if (s % HOUR === 0) return `${s / HOUR} 小时限额`;
  return `${Math.round(s / 60)} 分钟限额`;
}

/** "2 小时 14 分后重置", "明天 09:00 重置", "10月24日 周五 重置". */
export function resetText(resetsAt: string | null, now = Date.now()): string | undefined {
  if (!resetsAt) return undefined;
  const at = new Date(resetsAt);
  const ms = at.getTime() - now;
  if (ms <= 0) return "即将重置";
  const mins = Math.round(ms / 60_000);
  if (mins < 60) return `${mins} 分钟后重置`;
  if (mins < 24 * 60) {
    const h = Math.floor(mins / 60);
    const m = mins % 60;
    return m ? `${h} 小时 ${m} 分后重置` : `${h} 小时后重置`;
  }
  const hm = at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const days = Math.round((startOfDay(at) - startOfDay(new Date(now))) / 86_400_000);
  if (days === 1) return `明天 ${hm} 重置`;
  const week = "日一二三四五六"[at.getDay()];
  return `${at.getMonth() + 1}月${at.getDate()}日 周${week} 重置`;
}

const startOfDay = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();

export type Level = "ok" | "warn" | "danger";

/** Bars turn amber from 75% and red from 90% (or when the vendor says the limit is hit). */
export function level(usedPercent: number, reached = false): Level {
  if (reached || usedPercent >= 90) return "danger";
  if (usedPercent >= 75) return "warn";
  return "ok";
}

/** "Free", "Plus", "Pro", "Team"…: vendors send lower-case plan ids. */
export function planLabel(plan: string | null): string | undefined {
  if (!plan) return undefined;
  return plan
    .split(/[_\s-]+/)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

/** "53,035.30" from the vendor's decimal string. */
export function creditText(balance: string | null): string | undefined {
  if (balance == null) return undefined;
  const n = Number(balance);
  return Number.isFinite(n) ? n.toLocaleString(undefined, { maximumFractionDigits: 2 }) : balance;
}

/** The most used window across accounts, for a one-line summary. */
export function tightest<T extends { windows: RateWindow[] }>(accounts: T[]): { account: T; window: RateWindow } | undefined {
  let best: { account: T; window: RateWindow } | undefined;
  for (const account of accounts) {
    for (const window of account.windows) {
      if (!best || window.used_percent > best.window.used_percent) best = { account, window };
    }
  }
  return best;
}

/** How a session reached the vendor the last time the hub looked. */
export type EgressRoute = "proxy" | "direct" | "unknown";

/** The exit line under an account, in the words a person reads (docs/ACCOUNT-RISK.md「出口指纹」). */
export type Egress = {
  /** Whether the hub has ever confirmed an exit for this account. */
  known: boolean;
  ip: string;
  country: string;
  asn: string;
  route: EgressRoute;
  /** "经代理" / "直连" / "无法确认". */
  routeLabel: string;
  /** When the exit last changed, as the GUI reads times ("3 小时前", "11:02"). */
  changed?: string;
};

const ROUTE_LABEL: Record<EgressRoute, string> = {
  proxy: "经代理",
  direct: "直连",
  unknown: "无法确认",
};

/**
 * The exit line's values. A value the hub could not establish reads "未知" rather than
 * being left out: a failed lookup and an old good record are different things, and the
 * second is what the GUI shows until the next start can confirm the exit.
 */
export function egressOf(fingerprint: EgressFingerprint | undefined, now = Date.now()): Egress {
  const route: EgressRoute = fingerprint?.via_proxy === true ? "proxy" : fingerprint?.via_proxy === false ? "direct" : "unknown";
  return {
    known: !!fingerprint,
    ip: fingerprint?.ip ?? "IP 未知",
    country: fingerprint?.country ?? "国家未知",
    asn: fingerprint?.asn ?? "ASN 未知",
    route,
    routeLabel: ROUTE_LABEL[route],
    changed: fingerprint?.changed_at ? when(fingerprint.changed_at, new Date(now)) : undefined,
  };
}
