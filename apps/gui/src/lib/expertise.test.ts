import assert from "node:assert/strict";
import { test } from "node:test";
import {
  acceptCandidate,
  applyOutcome,
  budgetReport,
  conflictList,
  entryFlags,
  estimateTokens,
  expertiseCard,
  exportMarkdown,
  groupProfile,
  missingVerdicts,
  projectForWorkspace,
  staleScan,
  type Candidate,
  type EntryKind,
  type ExpertiseEntry,
  type UsageOutcome,
  type UsageRecord,
} from "./expertise.ts";

const NOW = new Date("2026-11-02T00:00:00Z");
const daysAgo = (d: number) => new Date(NOW.getTime() - d * 86_400_000).toISOString();

function entry(id: string, domain: string, kind: EntryKind, title: string, text: string): ExpertiseEntry {
  return {
    id,
    owner_kind: "person",
    owner_id: "me",
    domain,
    kind,
    title,
    text,
    example: null,
    applies_when: "",
    evidence: 0,
    last_confirmed_at: null,
    status: "active",
    superseded_by: null,
    stats: { used: 0, merged_first_try: 0, reworked: 0, dropped: 0 },
    sources: [],
    created_at: daysAgo(1),
    updated_at: daysAgo(1),
  };
}
function usage(id: string, entryId: string, days: number, outcome: UsageOutcome | null): UsageRecord {
  return { id, entry_id: entryId, run_id: null, task_id: null, agent: null, source: "pushed", outcome, at: daysAgo(days) };
}

test("tokens: one per CJK character, a quarter per other character", () => {
  assert.equal(estimateTokens(""), 0);
  assert.equal(estimateTokens("中文"), 2);
  assert.equal(estimateTokens("先跑测试再提交"), 7);
  assert.equal(estimateTokens("好，就这样"), 5);
  assert.equal(estimateTokens("abcd"), 1);
  assert.equal(estimateTokens("a"), 1);
  assert.equal(estimateTokens("中文 abc"), 3);
});

test("budget counts live judgements per domain and flags the one over 1500", () => {
  const gone = entry("g", "rust", "judgment", "旧", "x".repeat(8000));
  gone.status = "superseded";
  const budgets = budgetReport([
    entry("o", "rust", "judgment", "长篇", "x".repeat(6400)),
    gone,
    entry("a", "gui", "judgment", "短", ""),
    entry("l", "gui", "limit", "边界", "x".repeat(9000)),
  ]);
  assert.deepEqual(budgets.map((b) => b.domain), ["gui", "rust"]);
  assert.equal(budgets[0].judgment_tokens, 1);
  assert.equal(budgets[0].over, false);
  assert.equal(budgets[1].judgment_tokens, 2 + 1 + 1600);
  assert.equal(budgets[1].over, true);
  assert.equal(budgets[1].over_by, 103);
  assert.equal(budgets[1].judgment_count, 1);
  assert.deepEqual(budgets[1].merge_suggestions, []);
});

test("an over-budget domain suggests merging close judgements whose areas overlap", () => {
  const filler = "x".repeat(6000);
  const a = entry("a", "rust", "judgment", "cargo test before commit", filler);
  a.applies_when = "crates/**";
  const b = entry("b", "rust", "judgment", "cargo test before commit always", "");
  b.applies_when = "crates/hub";
  const c = entry("c", "rust", "judgment", "cargo test before commit", "");
  c.applies_when = "apps/gui";
  const [budget] = budgetReport([a, b, c]);
  assert.equal(budget.over, true);
  assert.deepEqual(budget.merge_suggestions, [
    { a: "a", b: "b", reason: "同一 domain，适用条件有交集，4 个用词相同" },
  ]);
});

test("stale scan: recent rework, high rate, then long unused", () => {
  const recent = entry("r", "rust", "judgment", "最近返工", "");
  recent.stats = { used: 2, merged_first_try: 1, reworked: 1, dropped: 0 };
  const rate = entry("h", "rust", "judgment", "返工多", "");
  rate.stats = { used: 3, merged_first_try: 2, reworked: 1, dropped: 0 };
  const old = entry("o", "rust", "judgment", "没人用", "");
  old.created_at = daysAgo(90);
  const fine = entry("f", "rust", "judgment", "好好的", "");
  const hints = staleScan(
    [recent, rate, old, fine],
    [usage("u1", "r", 3, "reworked"), usage("u2", "h", 45, "reworked"), usage("u3", "h", 2, "merged_first_try")],
    NOW,
  );
  assert.equal(hints.length, 3);
  assert.equal(hints[0].suggested, "review");
  assert.equal(hints[0].reason, "最近 30 天有返工（1/2 次）");
  assert.equal(hints[1].suggested, "stale");
  assert.equal(hints[1].reason, "返工率偏高（1/3 次）");
  assert.equal(hints[1].rework_rate, 0.333);
  assert.equal(hints[2].reason, "90 天没被用到（超过 60 天）");
  assert.equal(hints[2].rework_rate, null);
});

test("card compares the first-try rate with thirty days ago", () => {
  const usages = [
    usage("u1", "e", 2, "merged_first_try"),
    usage("u2", "e", 3, "merged_first_try"),
    usage("u3", "e", 4, "merged_first_try"),
    usage("u4", "e", 5, "reworked"),
    usage("u5", "e", 40, "merged_first_try"),
    usage("u6", "e", 41, "merged_first_try"),
    usage("u7", "e", 42, "reworked"),
    usage("u8", "e", 43, "reworked"),
    usage("u9", "e", 1, null),
  ];
  const stale = entry("s", "rust", "judgment", "待确认", "");
  stale.status = "stale";
  const review = entry("r", "rust", "judgment", "需复核", "");
  review.status = "review";
  const gone = entry("g", "rust", "judgment", "被替代", "");
  gone.status = "superseded";
  const card = expertiseCard(
    [stale, review, gone, entry("o", "rust", "judgment", "长篇判断", "x".repeat(6400)), entry("x", "gui", "example", "例子", ""), entry("l", "gui", "limit", "边界", "")],
    usages,
    NOW,
  );
  assert.equal(card.first_try_rate, 0.625);
  assert.equal(card.first_try_rate_30d_ago, 0.5);
  assert.equal(card.delta_points, 12.5);
  assert.equal(card.sample, 8);
  assert.equal(card.small_sample, true);
  assert.equal(card.merged_first_try, 5);
  assert.equal(card.reworked, 3);
  assert.equal(card.dropped, 0);
  assert.equal(card.entries, 6);
  assert.equal(card.judgments, 4);
  assert.equal(card.examples, 1);
  assert.equal(card.limits, 1);
  assert.equal(card.stale, 1);
  assert.equal(card.review, 1);
  assert.equal(card.superseded, 1);
  assert.equal(card.active, 3);
  assert.ok(card.judgment_tokens > 1500);
  assert.equal(card.over_budget_domains, 1);
});

test("card has no rate or delta without samples in both windows", () => {
  const none = expertiseCard([], [usage("u1", "e", 2, null)], NOW);
  assert.equal(none.first_try_rate, null);
  assert.equal(none.delta_points, null);
  assert.equal(none.sample, 0);
  const one = expertiseCard([], [usage("u1", "e", 2, "merged_first_try")], NOW);
  assert.equal(one.first_try_rate, 1);
  assert.equal(one.first_try_rate_30d_ago, null);
  assert.equal(one.delta_points, null);
});

test("export groups by domain then kind and marks warnings", () => {
  const example = entry("x", "rust", "example", "一次取舍", "正文");
  example.example = { situation: "要复用", rejected: "抽抽象", rejected_why: "只有两个调用点", chosen: "先复制" };
  const stale = entry("s", "rust", "judgment", "老判断", "因为这样");
  stale.status = "stale";
  stale.applies_when = "crates/**";
  const review = entry("r", "rust", "limit", "别这么做", "");
  review.status = "review";
  const gone = entry("g", "rust", "limit", "已替代", "");
  gone.status = "superseded";
  const text = exportMarkdown([example, stale, review, gone, entry("u", "gui", "judgment", "界面判断", "")]);
  assert.ok(text.startsWith("# 专长档案\n"));
  assert.ok(text.indexOf("## gui") < text.indexOf("## rust"));
  const j = text.indexOf("### 判断", text.indexOf("## rust"));
  assert.ok(j < text.indexOf("### 范例") && text.indexOf("### 范例") < text.indexOf("### 边界"));
  assert.ok(text.includes("- **老判断** — 因为这样（待确认）\n"));
  assert.ok(text.includes("  - 适用：`crates/**`\n"));
  assert.ok(text.includes("- **别这么做**（需复核）\n"));
  assert.ok(text.includes("  - 否掉的方案：抽抽象（原因：只有两个调用点）\n"));
  assert.ok(!text.includes("已替代"));
  const wrapped = exportMarkdown([entry("w", "rust", "judgment", "标题", "第一段\n\n第二段")]);
  assert.ok(wrapped.includes("- **标题** — 第一段 第二段\n"), "a blank line becomes one space");
  assert.equal(exportMarkdown([gone]), "");
});

test("group and flags: three kinds per domain, red for a high rework rate", () => {
  const bad = entry("b", "rust", "judgment", "坏", "");
  bad.stats = { used: 4, merged_first_try: 2, reworked: 2, dropped: 0 };
  const groups = groupProfile([bad, entry("x", "gui", "example", "例", "")]);
  assert.deepEqual(groups.map((g) => g.domain), ["gui", "rust"]);
  assert.deepEqual(groups[1].kinds.map((k) => k.entries.length), [1, 0, 0]);
  assert.equal(groups[0].budget, null);
  const flags = entryFlags(bad);
  assert.equal(flags[0].level, "red");
  assert.equal(flags[0].reason, "返工率 50%（2/4 次返工）");
});

test("outcomes update stats; accepting needs a verdict for every conflict", () => {
  const e = entry("e", "rust", "judgment", "先跑测试", "");
  const merged = applyOutcome(e, "merged_first_try", daysAgo(0));
  assert.deepEqual(merged.stats, { used: 1, merged_first_try: 1, reworked: 0, dropped: 0 });
  assert.equal(merged.evidence, 1);
  const dropped = applyOutcome(merged, "dropped", daysAgo(0));
  assert.equal(dropped.evidence, 1);
  assert.equal(dropped.stats.dropped, 1);

  const c: Candidate = {
    id: "c1",
    owner_kind: "person",
    owner_id: "me",
    domain: "rust",
    proposed: entry("p", "rust", "judgment", "先跑全量测试", ""),
    conflicts_with: ["e"],
    origin: "rework",
    status: "pending",
    suggested_by_lead: true,
    note: "返工说明",
    sources: [],
    created_at: daysAgo(0),
    decided_at: null,
  };
  assert.deepEqual(missingVerdicts(c, []), ["e"]);
  assert.throws(() => acceptCandidate([e], c, [], "n", daysAgo(0)));
  assert.equal(conflictList([c])[0].title, "先跑全量测试");
  assert.equal(conflictList([{ ...c, status: "accepted" }]).length, 0);

  const replaced = acceptCandidate([e], c, [{ entry_id: "e", verdict: "replace" }], "n", daysAgo(0));
  assert.equal(replaced.length, 2);
  assert.equal(replaced[0].status, "superseded");
  assert.equal(replaced[0].superseded_by, "n");
  assert.equal(replaced[1].id, "n");
  assert.deepEqual(replaced[1].stats, { used: 0, merged_first_try: 0, reworked: 0, dropped: 0 });
  assert.equal(replaced[1].evidence, 0);
  assert.equal(replaced[1].last_confirmed_at, null);
  const kept = acceptCandidate([e], c, [{ entry_id: "e", verdict: "keep_old" }], "n", daysAgo(0));
  assert.equal(kept.length, 2, "keep_old still adds the new entry");
  assert.equal(kept[0].status, "active");
  const old = { ...e, text: "旧的理由" };
  const merged2 = acceptCandidate([old], { ...c, proposed: { ...c.proposed, text: "新的理由" } }, [{ entry_id: "e", verdict: "merge" }], "n", daysAgo(0));
  assert.equal(merged2[0].status, "superseded");
  assert.equal(merged2[1].text, "新的理由\n\n旧的理由");
  const both = acceptCandidate([e], c, [{ entry_id: "e", verdict: "both" }], "n", daysAgo(0));
  assert.equal(both[0].status, "active");
  assert.equal(both.length, 2);
});

test("a workspace finds its project by checkout path, not by the path as a key", () => {
  const projects = [
    { id: "p1", name: "one", remotes: [], paths: [{ node: "local", path: "C:\\work\\one" }] },
    { id: "p2", name: "two", remotes: ["github.com/acme/two"], paths: [{ node: "local", path: "/home/me/two/" }] },
  ];
  assert.equal(projectForWorkspace(projects, "c:/work/one")?.id, "p1");
  assert.equal(projectForWorkspace(projects, "C:\\Work\\One\\")?.id, "p1");
  assert.equal(projectForWorkspace(projects, "/home/me/two")?.id, "p2");
  assert.equal(projectForWorkspace(projects, "c:/work/three"), undefined);
  assert.equal(projectForWorkspace(undefined, "c:/work/one"), undefined);
  assert.equal(projectForWorkspace(projects, ""), undefined);
});
