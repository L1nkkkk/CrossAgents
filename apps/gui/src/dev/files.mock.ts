// A small workspace for the file viewer in `preview:mock`: a few sources in several languages,
// a binary file, a file too big to show whole, and ignored directories listed only with `all`.
// MockBridge answers `files/tree`, `files/content` and `files/resolve` from it.

import type { FileContentResponse } from "@bindings/FileContentResponse";
import type { FileEntry } from "@bindings/FileEntry";
import type { FileTreeResponse } from "@bindings/FileTreeResponse";
import type { ResolvedPath } from "@bindings/ResolvedPath";

export const FILES_ROOT = "C:/work/notes";

type MockFile = { text: string } | { binary: number } | { big: number };

const MAIN_RS = `//! The \`notes\` command line: add, list and search notes.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

/// Where notes live unless \`--dir\` says otherwise.
const DEFAULT_DIR: &str = "~/.notes";

#[derive(Parser, Debug)]
struct Args {
    /// The notes directory.
    #[arg(long)]
    dir: Option<PathBuf>,
    /// Print one JSON object per line.
    #[arg(long)]
    json: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let dir = args.dir.unwrap_or_else(|| PathBuf::from(DEFAULT_DIR));
    let notes = notes::load(&dir).with_context(|| format!("reading {}", dir.display()))?;
    /* Newest first, like \`ls -t\`. */
    for (i, note) in notes.iter().rev().enumerate() {
        if args.json {
            println!("{}", serde_json::to_string(note)?);
        } else {
            println!("{:>3}  {}", i + 1, note.title);
        }
    }
    Ok(())
}
`;

const INDEX_TS = `// The web front of notes: one list, one search box.

import { render } from "./render";

export interface Note {
  id: string;
  title: string;
  tags: string[];
}

const API = "/api/notes";

export async function load(query = ""): Promise<Note[]> {
  const res = await fetch(\`\${API}?q=\${encodeURIComponent(query)}\`);
  if (!res.ok) throw new Error(\`HTTP \${res.status}\`);
  return (await res.json()) as Note[];
}

document.querySelector<HTMLInputElement>("#q")?.addEventListener("input", async (e) => {
  const notes = await load((e.target as HTMLInputElement).value);
  render(notes.filter((n) => n.tags.length > 0 || true));
});
`;

const CLI_MD = `# notes 命令行

\`notes\` 管理一个目录里的 Markdown 笔记。

## 选项

- \`--dir <路径>\`：笔记目录，默认 \`~/.notes\`。
- \`--json\`：所有命令都支持，输出一行一个 JSON 对象，适合脚本处理。

## 例子

\`\`\`sh
notes --json | jq .title
\`\`\`

更多见 [设计文档](DESIGN.md)。
`;

const CARGO_TOML = `[package]
name = "notes"
version = "0.3.1"
edition = "2021"

[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
serde_json = "1.0"
`;

const PACKAGE_JSON = `{
  "name": "notes-web",
  "private": true,
  "version": "0.3.1",
  "scripts": {
    "dev": "vite",
    "build": "tsc && vite build"
  },
  "devDependencies": {
    "typescript": "^5.6.0",
    "vite": "^6.0.0"
  }
}
`;

const BUILD_PY = `#!/usr/bin/env python3
"""Builds the CLI and the web front, then copies both into dist/."""

import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def run(*cmd: str) -> None:
    print("$", " ".join(cmd))
    subprocess.run(cmd, cwd=ROOT, check=True)


if __name__ == "__main__":
    run("cargo", "build", "--release")
    run("npm", "--prefix", "apps/web", "run", "build")
    shutil.copytree(ROOT / "apps/web/dist", ROOT / "dist/web", dirs_exist_ok=True)
    print(f"done: {ROOT / 'dist'}")  # 42 files
`;

const BIG = 3 * 1024 * 1024;
const bigText = (() => {
  const lines: string[] = [];
  for (let i = 0; lines.join("\n").length < 4000; i++) lines.push(`2026-10-27T09:${String(i % 60).padStart(2, "0")}:00Z INFO notes::sync pulled ${i * 7} notes`);
  return lines.join("\n") + "\n";
})();

/** Every file, by path relative to the root. Ignored ones start with `target/` or `node_modules/`. */
const FILES: Record<string, MockFile> = {
  ".gitignore": { text: "/target\nnode_modules/\n" },
  "Cargo.toml": { text: CARGO_TOML },
  "package.json": { text: PACKAGE_JSON },
  "crates/cli/src/main.rs": { text: MAIN_RS },
  "crates/cli/Cargo.toml": { text: CARGO_TOML.replace('name = "notes"', 'name = "notes-cli"') },
  "apps/web/src/index.ts": { text: INDEX_TS },
  "docs/cli.md": { text: CLI_MD },
  "docs/DESIGN.md": { text: "# 设计\n\n笔记就是目录里的 `.md` 文件，没有数据库。\n" },
  "scripts/build.py": { text: BUILD_PY },
  "assets/logo.png": { binary: 18_342 },
  "logs/sync.log": { big: BIG },
  "target/debug/notes.exe": { binary: 4_812_331 },
  "node_modules/vite/package.json": { text: '{ "name": "vite", "version": "6.0.3" }\n' },
};

const IGNORED = /^(target|node_modules)(\/|$)/;

function sizeOf(f: MockFile): number {
  return "text" in f ? new TextEncoder().encode(f.text).length : "binary" in f ? f.binary : f.big;
}

function isDir(path: string): boolean {
  return Object.keys(FILES).some((p) => p.startsWith(path + "/"));
}

/** Strips `./`, the root (absolute inputs) and backslashes; undefined when outside the root. */
function normalize(input: string): string | undefined {
  let p = input.replace(/\\/g, "/");
  const root = FILES_ROOT.toLowerCase();
  if (/^([a-zA-Z]:\/|\/)/.test(p)) {
    if (!p.toLowerCase().startsWith(root)) return undefined;
    p = p.slice(root.length);
  }
  p = p.replace(/^\/+/, "").replace(/^(\.\/)+/, "").replace(/\/+$/, "");
  return p;
}

export function mockTree(path: string, all: boolean): FileTreeResponse | undefined {
  const dir = normalize(path) ?? "";
  if (dir && !isDir(dir)) return undefined;
  const prefix = dir ? dir + "/" : "";
  const seen = new Map<string, FileEntry>();
  for (const [p, f] of Object.entries(FILES)) {
    if (!p.startsWith(prefix)) continue;
    const rest = p.slice(prefix.length);
    const name = rest.split("/")[0];
    const full = prefix + name;
    const ignored = IGNORED.test(full);
    if (ignored && !all) continue;
    if (seen.has(name)) continue;
    seen.set(name, rest.includes("/") ? { name, path: full, kind: "dir", ignored } : { name, path: full, kind: "file", size: sizeOf(f), ignored });
  }
  const entries = [...seen.values()].sort((a, b) => (a.kind === "dir" ? 0 : 1) - (b.kind === "dir" ? 0 : 1) || a.name.localeCompare(b.name));
  return { root: FILES_ROOT, path: dir, entries };
}

export function mockContent(path: string): FileContentResponse | undefined {
  const p = normalize(path);
  const f = p !== undefined ? FILES[p] : undefined;
  if (!p || !f) return undefined;
  const base = { path: p, abs_path: `${FILES_ROOT}/${p}`, size: sizeOf(f) };
  if ("binary" in f) return { ...base, binary: true, truncated: false };
  if ("big" in f) return { ...base, binary: false, truncated: true, text: bigText };
  return { ...base, binary: false, truncated: false, text: f.text };
}

export function mockResolve(paths: string[]): ResolvedPath[] {
  return paths.slice(0, 200).map((input) => {
    const p = normalize(input);
    if (!p) return { input };
    if (FILES[p]) return { input, path: p, abs_path: `${FILES_ROOT}/${p}`, kind: "file" };
    if (isDir(p)) return { input, path: p, abs_path: `${FILES_ROOT}/${p}`, kind: "dir" };
    return { input };
  });
}
