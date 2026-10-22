// How times read everywhere in the GUI (docs/tasks/V6-GUI.md §7). Pure; tested in time.test.ts.
//
// - Within the last hour: relative ("刚刚", "12 分钟前").
// - After that: the clock ("11:02", "昨天 18:40", "10-21 09:15", "2025-10-21 09:15").
// - On hover: the full time, or a start → end range.
//
// `now` is a parameter so the rules can be tested; callers leave it out.

const pad = (n: number) => String(n).padStart(2, "0");

function parse(iso: string | Date): Date | undefined {
  const d = iso instanceof Date ? iso : new Date(iso);
  return Number.isNaN(d.getTime()) ? undefined : d;
}

/** Whole days from `d`'s date to `now`'s date (local calendar): 0 today, 1 yesterday. */
function daysAgo(d: Date, now: Date): number {
  const day = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  return Math.round((day(now) - day(d)) / 86_400_000);
}

const hm = (d: Date) => `${pad(d.getHours())}:${pad(d.getMinutes())}`;
const md = (d: Date) => `${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
const ymd = (d: Date) => `${d.getFullYear()}-${md(d)}`;

/** "11:02", "昨天 18:40", "10-21 09:15", or with the year when it is not this year's. */
export function clock(iso: string | Date, now: Date = new Date()): string {
  const d = parse(iso);
  if (!d) return String(iso);
  const days = daysAgo(d, now);
  if (days === 0) return hm(d);
  if (days === 1) return `昨天 ${hm(d)}`;
  if (d.getFullYear() === now.getFullYear()) return `${md(d)} ${hm(d)}`;
  return `${ymd(d)} ${hm(d)}`;
}

/** The rule above: relative within the hour, then the clock. */
export function when(iso: string | Date, now: Date = new Date()): string {
  const d = parse(iso);
  if (!d) return String(iso);
  const s = (now.getTime() - d.getTime()) / 1000;
  if (s < 60) return "刚刚"; // also a clock running slightly ahead
  if (s < 3600) return `${Math.floor(s / 60)} 分钟前`;
  return clock(d, now);
}

/** "今天", "昨天", "10-21" (or with the year): headings that group a list by day. */
export function day(iso: string | Date, now: Date = new Date()): string {
  const d = parse(iso);
  if (!d) return String(iso);
  const days = daysAgo(d, now);
  if (days <= 0) return "今天";
  if (days === 1) return "昨天";
  return d.getFullYear() === now.getFullYear() ? md(d) : ymd(d);
}

/** "2026-10-22 11:02:13", for tooltips. */
export function full(iso: string | Date): string {
  const d = parse(iso);
  if (!d) return String(iso);
  return `${ymd(d)} ${hm(d)}:${pad(d.getSeconds())}`;
}

/** "2026-10-22 11:02:13 → 11:05:40" (the end's date only when it differs); open-ended without one. */
export function range(start: string | Date, end?: string | Date | null): string {
  const a = parse(start);
  if (!a) return String(start);
  const b = end ? parse(end) : undefined;
  if (!b) return `${full(a)} →`;
  const sameDay = ymd(a) === ymd(b);
  return `${full(a)} → ${sameDay ? `${hm(b)}:${pad(b.getSeconds())}` : full(b)}`;
}
