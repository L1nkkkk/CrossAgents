// How a review's findings read (docs/TODO.md §二十五 第 6 条): most severe first, the ones
// that block a merge (P0/P1) on top and unmistakable, with the confidence and the verdict.
// Pure, so it is tested (reviewFindings.test.ts) and shared by every place that shows a
// reviewer's result.

import type { ReviewFinding } from "@bindings/ReviewFinding";
import type { ReviewVerdict } from "@bindings/ReviewVerdict";

/** The severities a review may use, most severe first. */
export const SEVERITIES = ["P0", "P1", "P2", "P3"] as const;

export type SeverityTone = "danger" | "warn" | "muted";

const RANK: Record<string, number> = { P0: 0, P1: 1, P2: 2, P3: 3 };

/**
 * Findings most severe first (P0 → P3), blocking ones on top. A stable sort: findings of one
 * severity keep the reviewer's order. An unknown severity sorts last instead of vanishing.
 */
export function sortFindings(findings: readonly ReviewFinding[]): ReviewFinding[] {
  return [...findings].sort((a, b) => (RANK[a.severity] ?? 99) - (RANK[b.severity] ?? 99));
}

/** The findings that keep a change from merging: P0 must be fixed, P1 should be before the merge. */
export function blockingFindings(findings: readonly ReviewFinding[]): ReviewFinding[] {
  return sortFindings(findings).filter((f) => f.severity === "P0" || f.severity === "P1");
}

/** A severity's colour; P0/P1 must stand out, P2/P3 may recede. */
export function severityTone(severity: string): SeverityTone {
  if (severity === "P0") return "danger";
  if (severity === "P1") return "warn";
  return "muted";
}

/** The conclusion in a word, for the chip above the findings. */
export function verdictLabel(verdict: ReviewVerdict | null | undefined): string {
  switch (verdict) {
    case "merge":
      return "可以合并";
    case "merge_after_fixes":
      return "修完可合并";
    case "do_not_merge":
      return "不要合并";
    default:
      return "未给结论";
  }
}

/** A verdict's colour: green to merge, yellow for fixes, red against it. */
export function verdictTone(verdict: ReviewVerdict | null | undefined): "ok" | "warn" | "danger" | "muted" {
  switch (verdict) {
    case "merge":
      return "ok";
    case "merge_after_fixes":
      return "warn";
    case "do_not_merge":
      return "danger";
    default:
      return "muted";
  }
}

/** A confidence as a whole percent, clamped to 0–100 so a bad number cannot mislead. */
export function confidencePercent(confidence: number): number {
  if (!Number.isFinite(confidence)) return 0;
  return Math.round(Math.min(1, Math.max(0, confidence)) * 100);
}

/** Where a finding is, `path:line`, or just the path when it names no line. */
export function findingPlace(finding: Pick<ReviewFinding, "file" | "line">): string {
  if (!finding.file) return "";
  return finding.line ? `${finding.file}:${finding.line}` : finding.file;
}
