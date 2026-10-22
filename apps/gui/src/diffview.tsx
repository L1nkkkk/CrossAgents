// Diffs as the GUI shows them: the "+12 −4" stat and unified diffs, one block per file.

import { useEffect, useRef } from "react";
import { diffStat, type DiffFile } from "./lib/diff";

export function ChangeStat({ files }: { files: { added: number; removed: number }[] }) {
  const { added, removed } = diffStat(files);
  if (!added && !removed) return null;
  return (
    <span className="tiny mono change-stat">
      <span className="add">+{added}</span> <span className="del">−{removed}</span>
    </span>
  );
}

/** Unified diffs, one block per file; `focus` scrolls to that file and marks it. */
export function DiffView({ files, focus }: { files: DiffFile[]; focus?: string }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!focus) return;
    const el = ref.current?.querySelector(`[data-path="${CSS.escape(focus)}"]`);
    el?.scrollIntoView({ block: "start" });
  }, [focus, files]);
  return (
    <div className="diff" ref={ref}>
      {files.map((file, i) => (
        <div key={i} className={`diff-file ${focus === file.path ? "focus" : ""}`} data-path={file.path}>
          <div className="diff-head">
            <span className="mono">{file.path || "（未命名）"}</span>
            <ChangeStat files={[file]} />
          </div>
          <pre className="diff-body">
            {file.lines.map((line, j) => (
              <div key={j} className={`dl ${line.kind}`}>
                <span className="sign">{line.kind === "add" ? "+" : line.kind === "del" ? "−" : " "}</span>
                {line.text}
              </div>
            ))}
          </pre>
        </div>
      ))}
    </div>
  );
}
