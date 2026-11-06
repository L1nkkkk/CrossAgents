// Troubleshooting records (docs/tasks/LESSONS.md): labels and the small functions the page,
// the run's issue card and the mock hub share. The types are the hub's (bindings/).

import type { Lesson } from "@bindings/Lesson";
import type { LessonOutcome } from "@bindings/LessonOutcome";
import type { LessonStatus } from "@bindings/LessonStatus";
import type { RunIssue } from "@bindings/RunIssue";
import type { TriageRecord } from "@bindings/TriageRecord";

export type { Lesson, LessonOutcome, LessonStatus, RunIssue, TriageRecord };

export const STATUS_LABEL: Record<LessonStatus, string> = {
  active: "在用",
  review: "待复核",
  superseded: "已被替换",
};

export const OUTCOME_LABEL: Record<LessonOutcome, string> = {
  resolved: "已解决",
  workaround: "临时绕过",
  unresolved: "未解决",
};

/** The colour of an outcome or status tag (expertise.css: exp-tag yellow / red). */
export function outcomeTone(o: LessonOutcome): string {
  return o === "resolved" ? "" : o === "workaround" ? "yellow" : "red";
}

export const shortId = (id: string) => id.slice(0, 8);

/** What became of a recorded problem, in one line. */
export function triageText(t: TriageRecord | undefined): string {
  if (!t) return "未整理";
  switch (t.result) {
    case "lesson":
      return `成为排障记录 ${shortId(t.lesson_id)}`;
    case "merged":
      return `并入排障记录 ${shortId(t.lesson_id)}`;
    case "kept":
      return `不成记录：${t.why}`;
  }
}

/** The lesson a triaged issue went into, if any. */
export function triageLesson(t: TriageRecord | undefined): string | undefined {
  return t && (t.result === "lesson" || t.result === "merged") ? t.lesson_id : undefined;
}

/** An issue note's first line, without the 问题 label, for a one-line title. */
export function issueHeadline(text: string): string {
  const line = text.split("\n").find((l) => l.trim()) ?? "";
  return line.replace(/^\s*[-*#]*\s*(问题|symptom)\s*[:：]\s*/i, "").trim();
}

/** The message that asks the run's lead to sort its unsorted problems. */
export function triagePrompt(unsorted: number): string {
  return (
    `这个 run 还有 ${unsorted} 条记录的问题没整理（run 笔记 issue/ 下）。` +
    "请用 issue_pending 看一遍，逐条用 lesson_create、lesson_merge 或 issue_keep 整理。"
  );
}

/**
 * A plain substring match over a record's words, for the mock hub (the hub itself searches
 * with FTS5); every term of `q` has to be found.
 */
export function lessonMatches(l: Lesson, q: string): boolean {
  const hay = [l.title, l.symptom, l.conditions, l.resolution, ...l.keywords].join("\n").toLowerCase();
  return q
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every((term) => hay.includes(term));
}

/** The records as Markdown, the way `GET …/lessons/export` lays them out. */
export function exportLessons(list: Lesson[]): string {
  return list
    .map((l) =>
      [
        `## ${l.title}`,
        "",
        `- id: ${l.id}`,
        `- 状态: ${l.status} · 结果: ${l.outcome}`,
        l.keywords.length ? `- 关键词: ${l.keywords.join(", ")}` : "",
        l.conditions ? `- 条件: ${l.conditions}` : "",
        "",
        "### 症状",
        "",
        l.symptom,
        "",
        "### 处理",
        "",
        l.resolution,
        "",
      ]
        .filter((x, i, a) => x !== "" || a[i - 1] !== "")
        .join("\n"),
    )
    .join("\n");
}
