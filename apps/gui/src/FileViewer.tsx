// Read-only files: a lazily loaded tree and the file with line numbers and light highlighting; a
// Markdown file shows rendered, with its source a click away. Two places show them: the viewer
// over the page, opened from anywhere with `openFile({ scope, path, line })`, and the 文件 pane
// of the tool column (`FilesPane`), where the tree and the file take turns because it is narrow.
import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { ChevronDown, ChevronLeft, ChevronRight, Copy, ExternalLink, File, FileCode, Folder, FolderOpen, RefreshCw, X } from "lucide-react";
import type { FileContentResponse } from "@bindings/FileContentResponse";
import type { FileEntry } from "@bindings/FileEntry";
import { api, errorText, getBridge, scopeKey, scopePath, type FileScope } from "./api";
import { DocLinksProvider, FileScopeProvider, Prose } from "./components";
import { resolvePath } from "./fileResolve";
import { highlight, langOf, type Lang } from "./lib/highlight";
import "./files.css";

export type OpenFile = { scope: FileScope; path: string; line?: number };
type Current = OpenFile & { nonce: number };

let current: Current | null = null;
let nonce = 0;
const listeners = new Set<() => void>();
const changed = () => listeners.forEach((l) => l());

/** Opens the viewer on `path` (relative to the scope's workspace, or absolute inside it). */
export function openFile(target: OpenFile) {
  current = { ...target, nonce: ++nonce };
  changed();
}

export function closeFile() {
  current = null;
  changed();
}

function useCurrent() {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => current,
  );
}

const LANG_LABEL: Record<Lang, string> = { rs: "Rust", ts: "TypeScript", js: "JavaScript", json: "JSON", md: "Markdown", toml: "TOML", py: "Python", text: "纯文本" };

function scopeLabel(scope: FileScope): string {
  switch (scope.kind) {
    case "agent":
      return "会话工作目录";
    case "run":
      return "run 集成分支";
    case "task":
      return "任务工作树";
  }
}

const bytes = (n: number) => (n < 1024 ? `${n} B` : n < 1024 * 1024 ? `${(n / 1024).toFixed(1)} KB` : `${(n / 1024 / 1024).toFixed(1)} MB`);
const parentDirs = (path: string) => {
  const parts = path.split("/");
  return parts.slice(0, -1).map((_, i) => parts.slice(0, i + 1).join("/"));
};

/** Rendered once by App: the viewer, when a file is open. */
export function FileViewerHost() {
  const cur = useCurrent();
  if (!cur) return null;
  return <FileViewer key={scopeKey(cur.scope)} target={cur} />;
}

type Dir = FileEntry[] | "loading" | { error: string };
type FileState = FileContentResponse | "loading" | { error: string } | null;
/** A file asked for from outside the tree; `nonce` tells asking again from the same file. */
type Target = { path: string; line?: number; nonce: number };

/** The tree, the file shown and how they load, shared by the viewer and the pane. */
function useFileBrowser(scope: FileScope, target: Target | null, initial?: { open: Set<string>; showAll: boolean }) {
  const [showAll, setShowAll] = useState(initial?.showAll ?? false);
  const [dirs, setDirs] = useState<Map<string, Dir>>(new Map());
  const [open, setOpen] = useState<Set<string>>(initial?.open ?? new Set([""]));
  const [root, setRoot] = useState("");
  const [path, setPath] = useState<string | null>(null);
  const [line, setLine] = useState<number | undefined>(target?.line);
  const [file, setFile] = useState<FileState>(null);
  const [reloads, setReloads] = useState(0);

  const loadDir = (dir: string, all = showAll) => {
    setDirs((m) => new Map(m).set(dir, m.get(dir) && m.get(dir) !== "loading" ? m.get(dir)! : "loading"));
    api.filesTree(scope, dir, all).then(
      (r) => {
        if (dir === "") setRoot(r.root);
        setDirs((m) => new Map(m).set(dir, r.entries));
      },
      (e) => setDirs((m) => new Map(m).set(dir, { error: errorText(e) })),
    );
  };

  // The target: resolve it (it may be absolute or `./`), open its folders, load it.
  useEffect(() => {
    if (!target) return;
    let live = true;
    setLine(target.line);
    setFile("loading");
    resolvePath(scope, target.path).then((rel) => {
      if (!live) return;
      if (!rel) {
        setPath(target.path);
        setFile({ error: "文件不存在，或不在这个工作区内" });
        return;
      }
      setPath(rel);
      const ups = parentDirs(rel);
      setOpen((s) => new Set([...s, "", ...ups]));
      for (const d of ups) if (!dirs.has(d)) loadDir(d);
    });
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target?.nonce]);

  // The tree's root, and everything again when ignored files are shown or hidden.
  useEffect(() => {
    setDirs(new Map());
    for (const d of open) loadDir(d, showAll);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showAll]);

  // The file itself.
  useEffect(() => {
    if (!path) return;
    let live = true;
    setFile("loading");
    api.filesContent(scope, path).then(
      (r) => live && setFile(r),
      (e) => live && setFile({ error: errorText(e) }),
    );
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path, target?.nonce, reloads]);

  const toggle = (dir: string) => {
    setOpen((s) => {
      const next = new Set(s);
      if (next.has(dir)) next.delete(dir);
      else {
        next.add(dir);
        if (!dirs.has(dir)) loadDir(dir);
      }
      return next;
    });
  };

  const pick = (p: string) => {
    setLine(undefined);
    setPath(p);
  };

  /** Back to the tree alone. */
  const clear = () => {
    setPath(null);
    setLine(undefined);
    setFile(null);
  };

  /** The tree's open folders and the file again: an agent may have changed them. */
  const reload = () => {
    for (const d of open) loadDir(d);
    setReloads((n) => n + 1);
  };

  return { showAll, setShowAll, dirs, open, root, path, line, setLine, file, setFile, toggle, pick, clear, reload };
}

const loadedOf = (file: FileState) => (file && file !== "loading" && !("error" in file) ? file : null);

function FileViewer({ target }: { target: Current }) {
  const { scope } = target;
  const { showAll, setShowAll, dirs, open, root, path, line, setLine, file, setFile, toggle, pick } = useFileBrowser(scope, target);
  const [copied, setCopied] = useState(false);
  const bridge = getBridge();

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        closeFile();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const copy = () => {
    if (!path) return;
    navigator.clipboard?.writeText(path).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    });
  };

  const loaded = loadedOf(file);

  return (
    <div className="fv-backdrop" onMouseDown={(e) => e.target === e.currentTarget && closeFile()}>
      <div className="fv" role="dialog" aria-label="文件查看器">
        <header className="fv-head">
          <FileCode size={16} className="fv-head-icon" />
          <div className="fv-title">
            <div className="fv-path" title={loaded?.abs_path ?? path ?? ""}>
              {(path ?? target.path).split("/").map((seg, i, all) => (
                <span key={i} className={i === all.length - 1 ? "leaf" : ""}>
                  {seg}
                  {i < all.length - 1 && <span className="sep">/</span>}
                </span>
              ))}
              {line && <span className="fv-line-tag">第 {line} 行</span>}
            </div>
            <div className="fv-sub">
              {scopeLabel(scope)}
              {root && <> · {root}</>}
              {loaded && (
                <>
                  {" "}
                  · {bytes(loaded.size)}
                  {!loaded.binary && <> · {LANG_LABEL[langOf(loaded.path)]}</>}
                  {loaded.git_ref && <> · {loaded.git_ref}</>}
                </>
              )}
            </div>
          </div>
          <div className="fv-actions">
            <button className="ghost small" onClick={copy} disabled={!path} title="复制相对路径">
              <Copy size={14} /> {copied ? "已复制" : "复制路径"}
            </button>
            {bridge?.openPath && (
              <button
                className="ghost small"
                disabled={!loaded}
                title="用系统默认程序打开这个文件"
                onClick={() => path && bridge.openPath!(scopePath(scope), path).catch((e) => setFile({ error: errorText(e) }))}
              >
                <ExternalLink size={14} /> 用默认程序打开
              </button>
            )}
            <button className="icon fv-close" onClick={closeFile} title="关闭（Esc）" aria-label="关闭">
              <X size={16} />
            </button>
          </div>
        </header>
        <div className="fv-body">
          <nav className="fv-tree">
            <div className="fv-tree-head">
              <span>文件</span>
              <div className="fv-ignored" title="显示被 .gitignore 忽略的文件">
                <span>显示忽略的文件</span>
                <button className={`toggle ${showAll ? "on" : ""}`} role="switch" aria-checked={showAll} onClick={() => setShowAll((v) => !v)}>
                  <span />
                </button>
              </div>
            </div>
            <div className="fv-tree-scroll">
              <TreeLevel dir="" depth={0} dirs={dirs} open={open} selected={path} onToggle={toggle} onPick={pick} />
            </div>
          </nav>
          <section className="fv-code-pane">
            <FileBody scope={scope} file={file} line={line} onLine={setLine} nonce={target.nonce} onFollow={(p, l) => openFile({ scope, path: p, line: l })} />
          </section>
        </div>
      </div>
    </div>
  );
}

/** What the 文件 pane had open for a scope: switching tabs or conversations unmounts it. */
const remembered = new Map<string, { path: string | null; open: Set<string>; showAll: boolean }>();

/** The 文件 pane: the scope's tree; a file picked from it takes its place until going back. */
export function FilesPane({ scope }: { scope: FileScope }) {
  const key = scopeKey(scope);
  const [saved] = useState(() => remembered.get(key));
  const [target, setTarget] = useState<Target | null>(saved?.path ? { path: saved.path, nonce: 0 } : null);
  const { showAll, setShowAll, dirs, open, root, path, line, setLine, file, setFile, toggle, pick, clear, reload } = useFileBrowser(scope, target, saved);
  const bridge = getBridge();

  useEffect(() => {
    remembered.set(key, { path, open, showAll });
  }, [key, path, open, showAll]);

  const follow = (p: string, l?: number) => setTarget((t) => ({ path: p, line: l, nonce: (t?.nonce ?? 0) + 1 }));
  const refresh = (
    <button className="ghost icon" onClick={reload} title="重新读取" aria-label="重新读取">
      <RefreshCw size={14} />
    </button>
  );

  // A remembered file is on its way: `path` is not set until it has been resolved.
  if (!path && !target)
    return <FilesTree root={root} showAll={showAll} onShowAll={setShowAll} refresh={refresh} dirs={dirs} open={open} onToggle={toggle} onPick={pick} />;
  if (!path) return <div className="fv-empty">正在读取文件…</div>;

  const loaded = loadedOf(file);
  const slash = path.lastIndexOf("/");
  return (
    <div className="fp">
      <div className="fp-file-head">
        <button
          className="ghost icon"
          onClick={() => {
            clear();
            setTarget(null);
          }}
          title="回到文件列表"
          aria-label="回到文件列表"
        >
          <ChevronLeft size={15} />
        </button>
        <span className="fp-path" title={loaded?.abs_path ?? path}>
          {slash >= 0 && <span className="fp-dir">{path.slice(0, slash + 1)}</span>}
          <span className="leaf">{path.slice(slash + 1)}</span>
        </span>
        {refresh}
        {bridge?.openPath && (
          <button
            className="ghost icon"
            disabled={!loaded}
            title="用系统默认程序打开这个文件"
            aria-label="用系统默认程序打开"
            onClick={() => bridge.openPath!(scopePath(scope), path).catch((e) => setFile({ error: errorText(e) }))}
          >
            <ExternalLink size={14} />
          </button>
        )}
      </div>
      <section className="fv-code-pane">
        <FileBody scope={scope} file={file} line={line} onLine={setLine} nonce={target?.nonce ?? 0} onFollow={follow} />
      </section>
    </div>
  );
}

function FilesTree({
  root,
  showAll,
  onShowAll,
  refresh,
  dirs,
  open,
  onToggle,
  onPick,
}: {
  root: string;
  showAll: boolean;
  onShowAll: (update: (v: boolean) => boolean) => void;
  refresh: React.ReactNode;
  dirs: Map<string, Dir>;
  open: Set<string>;
  onToggle: (dir: string) => void;
  onPick: (path: string) => void;
}) {
  return (
    <div className="fp">
      <div className="fv-tree-head">
        <span className="fp-root" title={root}>
          {root.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || "文件"}
        </span>
        <span className="spacer" />
        <div className="fv-ignored" title="显示被 .gitignore 忽略的文件">
          <span>忽略的文件</span>
          <button className={`toggle ${showAll ? "on" : ""}`} role="switch" aria-checked={showAll} aria-label="显示被 .gitignore 忽略的文件" onClick={() => onShowAll((v) => !v)}>
            <span />
          </button>
        </div>
        {refresh}
      </div>
      <div className="fv-tree-scroll">
        <TreeLevel dir="" depth={0} dirs={dirs} open={open} selected={null} onToggle={onToggle} onPick={onPick} />
      </div>
    </div>
  );
}

function TreeLevel(props: {
  dir: string;
  depth: number;
  dirs: Map<string, Dir>;
  open: Set<string>;
  selected: string | null;
  onToggle: (dir: string) => void;
  onPick: (path: string) => void;
}) {
  const { dir, depth, dirs, open, selected, onToggle, onPick } = props;
  const entries = dirs.get(dir);
  const pad = { paddingLeft: 8 + depth * 14 };
  if (!entries || entries === "loading")
    return (
      <div className="fv-tree-note" style={pad}>
        加载中…
      </div>
    );
  if ("error" in entries)
    return (
      <div className="fv-tree-note err" style={pad}>
        {entries.error}
      </div>
    );
  if (entries.length === 0 && depth > 0)
    return (
      <div className="fv-tree-note" style={pad}>
        （空）
      </div>
    );
  return (
    <>
      {entries.map((e) => {
        const isDir = e.kind === "dir";
        const isOpen = isDir && open.has(e.path);
        return (
          <div key={e.path}>
            <button
              className={`fv-node ${selected === e.path ? "sel" : ""} ${e.ignored ? "ignored" : ""}`}
              style={pad}
              onClick={() => (isDir ? onToggle(e.path) : onPick(e.path))}
              title={e.ignored ? `${e.path}（被 .gitignore 忽略）` : e.path}
            >
              <span className="chev">{isDir ? isOpen ? <ChevronDown size={13} /> : <ChevronRight size={13} /> : null}</span>
              {isDir ? isOpen ? <FolderOpen size={14} className="ico dir" /> : <Folder size={14} className="ico dir" /> : <File size={14} className="ico" />}
              <span className="name">{e.name}</span>
            </button>
            {isOpen && <TreeLevel {...props} dir={e.path} depth={depth + 1} />}
          </div>
        );
      })}
    </>
  );
}

function FileBody({
  scope,
  file,
  line,
  onLine,
  nonce,
  onFollow,
}: {
  scope: FileScope;
  file: FileState;
  line?: number;
  onLine: (n: number) => void;
  nonce: number;
  /** A link in a rendered document was clicked; the path is from the workspace root. */
  onFollow: (path: string, line?: number) => void;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const text = file && file !== "loading" && !("error" in file) && !file.binary ? (file.text ?? "") : null;
  const lang = file && file !== "loading" && !("error" in file) ? langOf(file.path) : "text";
  const lines = useMemo(() => (text === null ? [] : highlight(text, lang)), [text, lang]);
  // A link to a line is about the source; otherwise the person's choice holds from file to file.
  const [choice, setSource] = useState<boolean | null>(null);
  useEffect(() => {
    if (line) setSource(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nonce]);
  const source = choice ?? !!line;
  const rendered = lang === "md" && !source;
  const filePath = loadedOf(file)?.path ?? "";
  const docLinks = { dir: filePath.slice(0, Math.max(0, filePath.lastIndexOf("/"))), open: onFollow };

  // Bring the wanted line into view once the file is there.
  useEffect(() => {
    if (!line || !lines.length) return;
    const el = scroller.current?.querySelector<HTMLElement>(`[data-line="${line}"]`);
    el?.scrollIntoView({ block: "center" });
  }, [line, lines, nonce, source]);

  if (!file || file === "loading") return <div className="fv-empty">正在读取文件…</div>;
  if ("error" in file) return <div className="fv-empty err">{file.error}</div>;
  if (file.binary)
    return (
      <div className="fv-empty">
        <strong>这是一个二进制文件</strong>
        <span>
          {bytes(file.size)}，无法以文本显示。{getBridge()?.openPath ? "可以用右上角的「用默认程序打开」查看。" : ""}
        </span>
      </div>
    );
  return (
    <div className="fv-code-wrap">
      {file.truncated && (
        <div className="fv-banner">
          文件较大（{bytes(file.size)}），只显示了前 1 MB 的内容。
        </div>
      )}
      {line && line > lines.length && <div className="fv-banner">文件只有 {lines.length} 行，没有第 {line} 行。</div>}
      {lang === "md" && (
        <div className="fv-md-bar">
          <div className="segmented small" role="tablist" aria-label="Markdown 显示方式">
            <button role="tab" aria-selected={rendered} className={rendered ? "active" : ""} onClick={() => setSource(false)}>
              预览
            </button>
            <button role="tab" aria-selected={!rendered} className={rendered ? "" : "active"} onClick={() => setSource(true)}>
              源码
            </button>
          </div>
        </div>
      )}
      {rendered ? (
        <div className="fv-md">
          <FileScopeProvider value={scope}>
            <DocLinksProvider value={docLinks}>
              <Prose doc text={text ?? ""} />
            </DocLinksProvider>
          </FileScopeProvider>
        </div>
      ) : (
      <div className="fv-code" ref={scroller}>
        <table>
          <tbody>
            {lines.map((toks, i) => (
              <tr key={i} data-line={i + 1} className={line === i + 1 ? "hit" : ""}>
                <td className="ln" onClick={() => onLine(i + 1)}>
                  {i + 1}
                </td>
                <td className="src">
                  {toks.length === 0
                    ? "\u200b"
                    : toks.map((t, j) =>
                        t.k === "plain" ? (
                          t.s
                        ) : (
                          <span key={j} className={`hl-${t.k}`}>
                            {t.s}
                          </span>
                        ),
                      )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      )}
    </div>
  );
}
