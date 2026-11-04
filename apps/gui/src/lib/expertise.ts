// Expertise profiles (docs EXPERTISE.md): the types mirror crates/core/src/expertise.rs, the
// functions mirror crates/orchestrator/src/expertise/maintain.rs so the GUI and its mock agree
// with the hub on budgets, warnings, the card and the export.

import type { Candidate } from "@bindings/Candidate";
import type { CandidateOrigin } from "@bindings/CandidateOrigin";
import type { ConflictDecision } from "@bindings/ConflictDecision";
import type { ConflictPair } from "@bindings/ConflictPair";
import type { ConflictVerdict } from "@bindings/ConflictVerdict";
import type { DomainBudget } from "@bindings/DomainBudget";
import type { EntryKind } from "@bindings/EntryKind";
import type { EntryStatus } from "@bindings/EntryStatus";
import type { ExpertiseCard } from "@bindings/ExpertiseCard";
import type { ExpertiseEntry } from "@bindings/ExpertiseEntry";
import type { ExpertiseMaintenance } from "@bindings/ExpertiseMaintenance";
import type { MergeSuggestion } from "@bindings/MergeSuggestion";
import type { ProjectInfo } from "@bindings/ProjectInfo";
import type { StaleHint } from "@bindings/StaleHint";

export type { AcceptCandidateRequest as AcceptRequest } from "@bindings/AcceptCandidateRequest";
export type { CandidateStatus } from "@bindings/CandidateStatus";
export type { EntryStats } from "@bindings/EntryStats";
export type { Example } from "@bindings/Example";
export type { OwnerKind } from "@bindings/OwnerKind";
export type { ProjectInfo } from "@bindings/ProjectInfo";
export type { SourceRef } from "@bindings/SourceRef";
export type {
  Candidate,
  CandidateOrigin,
  ConflictDecision,
  ConflictPair,
  ConflictVerdict,
  DomainBudget,
  EntryKind,
  EntryStatus,
  ExpertiseCard,
  ExpertiseEntry,
  ExpertiseMaintenance,
  MergeSuggestion,
  StaleHint,
};

export const PERSON_OWNER_ID = "me";

/** A path as a workspace key: separators unified, no trailing slash, lower case. */
const pathKey = (path: string) => path.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();

/**
 * The project a workspace belongs to. A project's profile is keyed by `Project.id`, not by
 * the directory it works in, so the client looks the directory up among the project's
 * checkouts first (`GET /v1/projects`).
 */
export function projectForWorkspace(projects: ProjectInfo[] | undefined, workspace: string): ProjectInfo | undefined {
  if (!workspace) return undefined;
  const key = pathKey(workspace);
  return projects?.find((project) => project.paths.some((at) => pathKey(at.path) === key));
}

export type UsageSource = "pushed" | "pulled" | "declared";
export type UsageOutcome = "merged_first_try" | "reworked" | "dropped";
export type UsageRecord = {
  id: string;
  entry_id: string;
  run_id: string | null;
  task_id: string | null;
  agent: string | null;
  source: UsageSource;
  outcome: UsageOutcome | null;
  at: string;
};

export type Verdict = ConflictVerdict;
export type Maintenance = ExpertiseMaintenance;

export const JUDGMENT_BUDGET = 1500;
export const STALE_AFTER_DAYS = 60;
export const RECENT_FAILURE_DAYS = 30;
export const REWORK_RATE_MIN_SAMPLE = 3;
export const REWORK_RATE_THRESHOLD = 0.3;
export const CARD_WINDOW_DAYS = 30;
const DAY = 86_400_000;

export const KINDS: EntryKind[] = ["judgment", "example", "limit"];
export const KIND_LABEL: Record<EntryKind, string> = { judgment: "判断", example: "范例", limit: "边界" };
export const VERDICT_LABEL: Record<Verdict, string> = { keep_old: "保留旧的", replace: "替换", merge: "合并", both: "都留" };
export const ORIGIN_LABEL: Record<CandidateOrigin, string> = {
  rework: "返工",
  review: "评审",
  merged_first_try: "一次合并",
  manual: "手写",
};

function isCjk(cp: number): boolean {
  return (
    (cp >= 0x3000 && cp <= 0x303f) ||
    (cp >= 0x3400 && cp <= 0x4dbf) ||
    (cp >= 0x4e00 && cp <= 0x9fff) ||
    (cp >= 0xf900 && cp <= 0xfaff) ||
    (cp >= 0xff00 && cp <= 0xffef) ||
    (cp >= 0x20000 && cp <= 0x2fa1f)
  );
}

/** Rough token count: one per CJK character, one per four other characters (rounded up). */
export function estimateTokens(text: string): number {
  let cjk = 0;
  let other = 0;
  for (const ch of text) {
    if (isCjk(ch.codePointAt(0)!)) cjk++;
    else other++;
  }
  return cjk + Math.ceil(other / 4);
}

const roundTo = (n: number, places: number) => {
  const f = 10 ** places;
  return Math.round(n * f) / f;
};
const rate = (n: number, d: number) => (d > 0 ? roundTo(n / d, 3) : null);
const oneLine = (s: string) => s.replace(/\s+/g, " ").trim();
const byText = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);

/** The text of a judgement as it is pushed: title, then text when there is one. */
export function judgmentCopy(e: ExpertiseEntry): string {
  return e.text.trim() ? `${e.title} ${e.text}` : e.title;
}

/** Judgement tokens per domain against the 1500 budget; superseded entries do not count. */
export function budgetReport(entries: ExpertiseEntry[]): DomainBudget[] {
  const by = new Map<string, ExpertiseEntry[]>();
  for (const e of entries) {
    if (e.kind !== "judgment" || e.status === "superseded") continue;
    by.set(e.domain, [...(by.get(e.domain) ?? []), e]);
  }
  return [...by.keys()].sort(byText).map((domain) => {
    const list = by.get(domain)!;
    const tokens = list.reduce((n, e) => n + estimateTokens(judgmentCopy(e)), 0);
    const over = tokens > JUDGMENT_BUDGET;
    return {
      domain,
      judgment_tokens: tokens,
      budget: JUDGMENT_BUDGET,
      over,
      over_by: Math.max(0, tokens - JUDGMENT_BUDGET),
      judgment_count: list.length,
      merge_suggestions: over ? mergeSuggestions(list) : [],
    };
  });
}

/** Pairs of judgements in one domain whose applicability overlaps and whose wording is close. */
export function mergeSuggestions(judgments: ExpertiseEntry[]): MergeSuggestion[] {
  const items = judgments.map((e) => ({ e, w: words(judgmentCopy(e)) }));
  const out: MergeSuggestion[] = [];
  items.forEach((left, i) => {
    for (const right of items.slice(i + 1)) {
      if (!appliesOverlap(left.e.applies_when, right.e.applies_when)) continue;
      let shared = 0;
      for (const w of left.w) if (right.w.has(w)) shared++;
      const smaller = Math.min(left.w.size, right.w.size);
      if (shared < 3 || shared * 2 < smaller) continue;
      out.push({ a: left.e.id, b: right.e.id, reason: `同一 domain，适用条件有交集，${shared} 个用词相同` });
    }
  });
  return out;
}

/** Do two `applies_when` values cover anything in common? Empty means anywhere. */
export function appliesOverlap(a: string, b: string): boolean {
  const left = scopes(a);
  const right = scopes(b);
  if (!left.length || !right.length) return true;
  const under = (p: string, parent: string) => p.startsWith(parent) && p.slice(parent.length).startsWith("/");
  return left.some((l) => right.some((r) => l === r || under(l, r) || under(r, l)));
}

function scopes(value: string): string[] {
  return value
    .split(/[,;\s]/)
    .filter(Boolean)
    .map((p) => p.replace(/(\*\*)+$/, "").replace(/\/+$/, "").toLowerCase())
    .filter(Boolean);
}

/** Latin runs of three or more characters, and CJK bigrams (a lone CJK character on its own). */
function words(text: string): Set<string> {
  const set = new Set<string>();
  let latin = "";
  let cjk: string[] = [];
  const flushLatin = () => {
    if (latin.length >= 3) set.add(latin);
    latin = "";
  };
  const flushCjk = () => {
    if (cjk.length === 1) set.add(cjk[0]);
    else for (let i = 0; i + 1 < cjk.length; i++) set.add(cjk[i] + cjk[i + 1]);
    cjk = [];
  };
  for (const ch of text.toLowerCase()) {
    if (isCjk(ch.codePointAt(0)!)) {
      flushLatin();
      cjk.push(ch);
    } else if (/^[a-z0-9_]$/.test(ch)) {
      flushCjk();
      latin += ch;
    } else {
      flushLatin();
      flushCjk();
    }
  }
  flushLatin();
  flushCjk();
  return set;
}

/** Active entries that should turn yellow (stale or review), and why. */
export function staleScan(entries: ExpertiseEntry[], usages: UsageRecord[], now: Date): StaleHint[] {
  const hints: StaleHint[] = [];
  const recent = now.getTime() - RECENT_FAILURE_DAYS * DAY;
  for (const e of entries) {
    if (e.status !== "active") continue;
    const mine = usages.filter((u) => u.entry_id === e.id);
    const seen = Math.max(
      Date.parse(e.created_at),
      e.last_confirmed_at ? Date.parse(e.last_confirmed_at) : 0,
      ...mine.map((u) => Date.parse(u.at)),
    );
    const daysUnused = Math.max(0, Math.floor((now.getTime() - seen) / DAY));
    const { used, reworked } = e.stats;
    const reworkRate = used > 0 ? roundTo(reworked / used, 3) : null;
    const base = { entry_id: e.id, domain: e.domain, title: e.title, days_unused: daysUnused, rework_rate: reworkRate };
    if (reworked > 0 && mine.some((u) => u.outcome === "reworked" && Date.parse(u.at) >= recent)) {
      hints.push({ ...base, suggested: "review", reason: `最近 ${RECENT_FAILURE_DAYS} 天有返工（${reworked}/${used} 次）` });
    } else if (used >= REWORK_RATE_MIN_SAMPLE && (reworkRate ?? 0) >= REWORK_RATE_THRESHOLD) {
      hints.push({ ...base, suggested: "stale", reason: `返工率偏高（${reworked}/${used} 次）` });
    } else if (daysUnused >= STALE_AFTER_DAYS) {
      hints.push({ ...base, suggested: "stale", reason: `${daysUnused} 天没被用到（超过 ${STALE_AFTER_DAYS} 天）` });
    }
  }
  return hints;
}

/** One pair per pending candidate and entry it conflicts with. */
export function conflictList(candidates: Candidate[]): ConflictPair[] {
  const pairs: ConflictPair[] = [];
  for (const c of candidates) {
    if (c.status !== "pending") continue;
    for (const id of c.conflicts_with) {
      pairs.push({ candidate_id: c.id, entry_id: id, domain: c.domain, title: c.proposed.title, note: c.note });
    }
  }
  return pairs;
}

/** The expertise card: first-try rate now and 30 days ago, counts, judgement tokens. */
export function expertiseCard(entries: ExpertiseEntry[], usages: UsageRecord[], now: Date): ExpertiseCard {
  const cutoff = now.getTime() - CARD_WINDOW_DAYS * DAY;
  const all = { merged_first_try: 0, reworked: 0, dropped: 0 };
  const then = { merged_first_try: 0, reworked: 0, dropped: 0 };
  for (const u of usages) {
    if (!u.outcome) continue;
    all[u.outcome]++;
    if (Date.parse(u.at) < cutoff) then[u.outcome]++;
  }
  const sample = all.merged_first_try + all.reworked + all.dropped;
  const sampleThen = then.merged_first_try + then.reworked + then.dropped;
  const first = rate(all.merged_first_try, sample);
  const before = rate(then.merged_first_try, sampleThen);
  const budgets = budgetReport(entries);
  const kind = (k: EntryKind) => entries.filter((e) => e.kind === k).length;
  const status = (s: EntryStatus) => entries.filter((e) => e.status === s).length;
  return {
    first_try_rate: first,
    first_try_rate_30d_ago: before,
    delta_points: first !== null && before !== null ? roundTo((first - before) * 100, 1) : null,
    sample,
    small_sample: sample < 10,
    ...all,
    entries: entries.length,
    judgments: kind("judgment"),
    examples: kind("example"),
    limits: kind("limit"),
    active: status("active"),
    stale: status("stale"),
    review: status("review"),
    superseded: status("superseded"),
    judgment_tokens: budgets.reduce((n, b) => n + b.judgment_tokens, 0),
    judgment_budget: JUDGMENT_BUDGET,
    over_budget_domains: budgets.filter((b) => b.over).length,
  };
}

/** The profile as Markdown: by domain, then kind; empty when nothing is left to export. */
export function exportMarkdown(entries: ExpertiseEntry[]): string {
  const live = entries.filter((e) => e.status !== "superseded");
  if (!live.length) return "";
  let out = "# 专长档案\n";
  for (const domain of [...new Set(live.map((e) => e.domain))].sort(byText)) {
    out += `\n## ${domain}\n`;
    for (const kind of KINDS) {
      const list = live.filter((e) => e.domain === domain && e.kind === kind);
      if (!list.length) continue;
      out += `\n### ${KIND_LABEL[kind]}\n\n`;
      for (const e of list) {
        out += `- **${oneLine(e.title)}**`;
        const text = oneLine(e.text);
        if (e.text) out += ` — ${text}`;
        if (e.status === "stale") out += "（待确认）";
        if (e.status === "review") out += "（需复核）";
        out += "\n";
        if (e.example) {
          const x = e.example;
          if (x.situation) out += `  - 情况：${oneLine(x.situation)}\n`;
          if (x.rejected) {
            out += `  - 否掉的方案：${oneLine(x.rejected)}`;
            if (x.rejected_why) out += `（原因：${oneLine(x.rejected_why)}）`;
            out += "\n";
          }
          if (x.chosen) out += `  - 采用：${oneLine(x.chosen)}\n`;
        }
        if (e.applies_when) out += `  - 适用：\`${oneLine(e.applies_when)}\`\n`;
      }
    }
  }
  return out;
}

export type ProfileGroup = {
  domain: string;
  budget: DomainBudget | null;
  kinds: { kind: EntryKind; entries: ExpertiseEntry[] }[];
};

/** Entries of one owner by domain (sorted), each split into the three kinds. */
export function groupProfile(entries: ExpertiseEntry[]): ProfileGroup[] {
  const budgets = budgetReport(entries);
  return [...new Set(entries.map((e) => e.domain))].sort(byText).map((domain) => ({
    domain,
    budget: budgets.find((b) => b.domain === domain) ?? null,
    kinds: KINDS.map((kind) => ({ kind, entries: entries.filter((e) => e.domain === domain && e.kind === kind) })),
  }));
}

export type EntryFlag = { level: "yellow" | "red"; reason: string };

/** Why an entry is marked: yellow for stale/review, red for a high rework rate. */
export function entryFlags(e: ExpertiseEntry, hints: StaleHint[] = []): EntryFlag[] {
  const flags: EntryFlag[] = [];
  const { used, reworked } = e.stats;
  if (used >= REWORK_RATE_MIN_SAMPLE && reworked / used >= REWORK_RATE_THRESHOLD) {
    flags.push({ level: "red", reason: `返工率 ${Math.round((reworked / used) * 100)}%（${reworked}/${used} 次返工）` });
  }
  if (e.status === "stale") flags.push({ level: "yellow", reason: "待确认" });
  if (e.status === "review") flags.push({ level: "yellow", reason: "需复核" });
  for (const h of hints) {
    if (h.entry_id === e.id && e.status === "active") {
      flags.push({ level: "yellow", reason: `建议${h.suggested === "review" ? "复核" : "标为待确认"}：${h.reason}` });
    }
  }
  return flags;
}

/** The conflicts of a candidate that have no verdict yet; accepting needs this empty. */
export function missingVerdicts(c: Candidate, decisions: ConflictDecision[]): string[] {
  return c.conflicts_with.filter((id) => !decisions.some((d) => d.entry_id === id));
}

/** An entry's stats after one decided usage. */
export function applyOutcome(e: ExpertiseEntry, outcome: UsageOutcome, at: string): ExpertiseEntry {
  const stats = { ...e.stats, used: e.stats.used + 1 };
  stats[outcome]++;
  const confirmed = outcome !== "dropped";
  return {
    ...e,
    stats,
    evidence: e.evidence + (confirmed ? 1 : 0),
    last_confirmed_at: confirmed ? at : e.last_confirmed_at,
    updated_at: at,
  };
}

/** Accept a candidate as the hub does: always a new entry (fresh stats, no evidence yet); old
 * entries with replace/merge are superseded by it, and merge appends their text to it.
 * Throws when a conflict has no verdict. */
export function acceptCandidate(
  entries: ExpertiseEntry[],
  c: Candidate,
  decisions: ConflictDecision[],
  newId: string,
  at: string,
): ExpertiseEntry[] {
  const missing = missingVerdicts(c, decisions);
  if (missing.length) throw new Error(`冲突没有裁决：${missing.join(", ")}`);
  let text = c.proposed.text;
  for (const id of c.conflicts_with) {
    const old = entries.find((e) => e.id === id);
    const kept = old?.text.trim() ?? "";
    const merge = decisions.find((d) => d.entry_id === id)?.verdict === "merge";
    if (merge && kept && !text.includes(kept)) text = `${text.trimEnd()}\n\n${kept}`;
  }
  const next = entries.map((e) => {
    const d = decisions.find((x) => x.entry_id === e.id);
    if (!d || !c.conflicts_with.includes(e.id) || (d.verdict !== "replace" && d.verdict !== "merge")) return e;
    return { ...e, status: "superseded" as const, superseded_by: newId, updated_at: at };
  });
  return [
    ...next,
    {
      ...c.proposed,
      id: newId,
      owner_kind: c.owner_kind,
      owner_id: c.owner_id,
      domain: c.domain,
      text,
      sources: [
        ...c.sources,
        ...c.proposed.sources.filter((s) => !c.sources.some((x) => JSON.stringify(x) === JSON.stringify(s))),
      ],
      evidence: 0,
      last_confirmed_at: null,
      status: "active",
      superseded_by: null,
      stats: { used: 0, merged_first_try: 0, reworked: 0, dropped: 0 },
      created_at: at,
      updated_at: at,
    },
  ];
}
