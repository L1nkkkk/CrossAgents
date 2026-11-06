// Where a run's lead works (docs/tasks/LEAD.md §2, §11): in the person's own directory, or in a
// checkout the hub made. The two look alike in a conversation, so every place a run is named —
// the run page's header, a sidebar row — says which one it is, and the tooltip says what that
// means for branches, landing and the person's files.

import type { Run } from "@bindings/Run";
import type { RunMode } from "@bindings/RunMode";
import { Icon } from "./components";

/** The mode a run works in; runs recorded before the split carry only `in_place`. */
export function modeOf(run: Pick<Run, "mode" | "in_place">): RunMode {
  return (
    run.mode ?? {
      lead: run.in_place ? "in_place" : "worktree",
      vcs: run.in_place ? "none" : "git",
      owner: run.in_place ? "person" : "hub",
      retain: "keep",
    }
  );
}

/** The lead works in the person's own directory. */
export const leadsInPlace = (run: Pick<Run, "mode" | "in_place">) => modeOf(run).lead === "in_place";

/** What the tag reads, and what its tooltip says it means. */
export function modeWords(run: Pick<Run, "mode" | "in_place">): { label: string; title: string } {
  const mode = modeOf(run);
  if (mode.lead === "in_place") {
    const vcs = mode.vcs === "git" ? "这里不建分支、不自动提交" : "这个目录不在 Git 下，也没有分支";
    return {
      label: "原地",
      title: `原地：主导在你的目录里直接改，不搬家。${vcs}；Worker 的改动由 hub 应用回来，跑完工作区还在原处。`,
    };
  }
  return {
    label: "worktree",
    title: "worktree：主导在 hub 建的集成工作区里改，每轮一个分支，落地时合进基线；跑完工作区保留，下一轮在同一个工作区接着干。",
  };
}

/**
 * The one-line tag. `compact` only drops padding for tight rows; the tooltip keeps the
 * explanation either way, so a narrow sidebar still tells the two apart.
 */
export function RunModeTag({ run, compact = false, className = "" }: { run: Pick<Run, "mode" | "in_place">; compact?: boolean; className?: string }) {
  const { label, title } = modeWords(run);
  const place = modeOf(run).lead;
  return (
    <span
      className={`chip mode-tag ${place === "in_place" ? "in-place" : "worktree"} ${compact ? "compact" : ""} ${className}`.trim()}
      title={title}
      data-mode={place}
    >
      {label}
    </span>
  );
}

/**
 * The same fact where a row has no room for words: one small glyph — a folder for the person's
 * own directory, a branch for the hub's worktree — with the whole explanation as its tooltip.
 */
export function RunModeMark({ run, className = "" }: { run: Pick<Run, "mode" | "in_place">; className?: string }) {
  const { label, title } = modeWords(run);
  const place = modeOf(run).lead;
  return (
    <span
      className={`mode-mark ${place === "in_place" ? "in-place" : "worktree"} ${className}`.trim()}
      title={title}
      role="img"
      aria-label={label}
      data-mode={place}
    >
      <Icon name={place === "in_place" ? "folder" : "branch"} size={11} />
    </span>
  );
}
