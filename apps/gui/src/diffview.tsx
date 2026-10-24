// Diffs as the GUI shows them: the "+12 −4" stat and unified diffs, one block per file.

import { useEffect, useRef, type ReactNode } from "react";
import { diffStat, type DiffFile, type DiffLine } from "./lib/diff";

export function ChangeStat({ files }: { files: { added: number; removed: number }[] }) {
  const { added, removed } = diffStat(files);
  if (!added && !removed) return null;
  return (
    <span className="tiny mono change-stat">
      <span className="add">+{added}</span> <span className="del">−{removed}</span>
    </span>
  );
}

/**
 * What a review adds to a diff (docs/tasks/DOGFOOD.md §2): line numbers that open a comment
 * on that line, a button on each file's head for a comment on the whole file, and whatever
 * goes under a line or a file head (drafts, the open editor).
 */
export type DiffReview = {
  onLine: (path: string, line: DiffLine) => void;
  onFile: (path: string) => void;
  below: (path: string, line?: DiffLine) => ReactNode;
};

/** Unified diffs, one block per file; `focus` scrolls to that file and marks it. */
export function DiffView({ files, focus, review }: { files: DiffFile[]; focus?: string; review?: DiffReview }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!focus) return;
    const el = ref.current?.querySelector(`[data-path="${CSS.escape(focus)}"]`);
    el?.scrollIntoView({ block: "start" });
  }, [focus, files]);
  return (
    <div className={`diff ${review ? "reviewing" : ""}`} ref={ref}>
      {files.map((file, i) => (
        <div key={i} className={`diff-file ${focus === file.path ? "focus" : ""}`} data-path={file.path}>
          <div className="diff-head">
            <span className="mono">{file.path || "（未命名）"}</span>
            <span className="row" style={{ gap: 8 }}>
              <ChangeStat files={[file]} />
              {review && file.path && (
                <button className="link tiny" onClick={() => review.onFile(file.path)} title="对整个文件提意见">
                  评论文件
                </button>
              )}
            </span>
          </div>
          {review?.below(file.path)}
          <pre className="diff-body">
            {file.lines.map((line, j) => (
              <div key={j}>
                <div className={`dl ${line.kind}`}>
                  {review && <LineNo line={line} onClick={() => review.onLine(file.path, line)} />}
                  <span className="sign">{line.kind === "add" ? "+" : line.kind === "del" ? "−" : " "}</span>
                  {line.text}
                </div>
                {review?.below(file.path, line)}
              </div>
            ))}
          </pre>
        </div>
      ))}
    </div>
  );
}

/** The line's number (new file, or old for a removed line); a button that opens a comment. */
function LineNo({ line, onClick }: { line: DiffLine; onClick: () => void }) {
  const n = line.new ?? line.old;
  if (n === undefined) return <span className="ln" />;
  return (
    <button className="ln" onClick={onClick} title="在这一行提意见">
      {n}
    </button>
  );
}
