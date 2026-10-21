// Unified diffs as files and lines, for the diff view. Pure, tested in diff.test.ts.

export type DiffLine = { kind: "add" | "del" | "ctx" | "hunk" | "meta"; text: string };
export type DiffFile = { path: string; added: number; removed: number; lines: DiffLine[] };

/** Splits `git diff` output (one or more files) into files. A bare hunk list becomes one file named `fallback`. */
export function parseDiff(text: string, fallback = ""): DiffFile[] {
  const files: DiffFile[] = [];
  let file: DiffFile | undefined;
  const start = (path: string) => {
    file = { path, added: 0, removed: 0, lines: [] };
    files.push(file);
  };
  for (const raw of text.replace(/\r\n/g, "\n").split("\n")) {
    if (raw.startsWith("diff --git ")) {
      const m = / b\/(.+)$/.exec(raw);
      start(m ? m[1] : raw.slice(11));
      continue;
    }
    if (!file) start(fallback);
    const f = file!;
    if (raw.startsWith("+++ ")) {
      const path = raw.slice(4).replace(/^b\//, "");
      if (path !== "/dev/null" && !f.path) f.path = path;
      continue;
    }
    if (raw.startsWith("--- ")) {
      const path = raw.slice(4).replace(/^a\//, "");
      if (path !== "/dev/null" && !f.path) f.path = path;
      continue;
    }
    if (/^(index |new file mode|deleted file mode|similarity index|rename (from|to) |old mode|new mode)/.test(raw)) {
      f.lines.push({ kind: "meta", text: raw });
    } else if (raw.startsWith("@@")) {
      f.lines.push({ kind: "hunk", text: raw });
    } else if (raw.startsWith("+")) {
      f.added++;
      f.lines.push({ kind: "add", text: raw.slice(1) });
    } else if (raw.startsWith("-")) {
      f.removed++;
      f.lines.push({ kind: "del", text: raw.slice(1) });
    } else if (raw.startsWith("\\")) {
      f.lines.push({ kind: "meta", text: raw });
    } else if (raw.length || f.lines.length) {
      f.lines.push({ kind: "ctx", text: raw.startsWith(" ") ? raw.slice(1) : raw });
    }
  }
  // Drop the trailing empty context line a final newline leaves.
  for (const f of files) {
    const last = f.lines[f.lines.length - 1];
    if (last?.kind === "ctx" && last.text === "") f.lines.pop();
  }
  return files.filter((f) => f.path || f.lines.length);
}

/** "+12 −4" over all files. */
export function diffStat(files: DiffFile[]): { files: number; added: number; removed: number } {
  return {
    files: files.length,
    added: files.reduce((n, f) => n + f.added, 0),
    removed: files.reduce((n, f) => n + f.removed, 0),
  };
}

/** The last `max` lines of `text`, and how many were left out. */
export function tailLines(text: string, max: number): { text: string; hidden: number } {
  const lines = text.replace(/\n$/, "").split("\n");
  if (lines.length <= max) return { text: lines.join("\n"), hidden: 0 };
  return { text: lines.slice(-max).join("\n"), hidden: lines.length - max };
}
