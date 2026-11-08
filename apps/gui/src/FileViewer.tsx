// Read-only files: a lazily loaded tree and the file with line numbers and light highlighting; a
// Markdown file shows rendered, with its source a click away. Two places show them: the viewer
// over the page, opened from anywhere with `openFile({ scope, path, line })`, and the 文件 pane
// of the tool column (`FilesPane`), which puts its tabs and content beside the tree when its own
// width allows and falls back to showing them in turn when it does not.
import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { ChevronDown, ChevronLeft, ChevronRight, Copy, ExternalLink, File, FileCode, Folder, FolderOpen, List, Maximize2, RefreshCw, Search, X } from "lucide-react";
import type { FileContentResponse } from "@bindings/FileContentResponse";
import type { FileEntry } from "@bindings/FileEntry";
import type { FileStatusEntry } from "@bindings/FileStatusEntry";
import { api, errorText, getBridge, scopeKey, scopePath, type FileScope } from "./api";
import { DocLinksProvider, FileScopeProvider, Prose } from "./components";
import { resolvePath } from "./fileResolve";
import { highlight, langOf, type Lang } from "./lib/highlight";
import { baseOf, changeIndex, changeMark, currentHeading, dirOf, outlineOf, type ChangeMark, type Heading } from "./lib/files";
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
const NO_MARKS: ChangeMark = { files: new Map(), dirs: new Set() };
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
                onClick={() => path && bridge.openPath!(scopePath(scope), path).then(({ copy }) => { if (copy) window.alert("打开的是副本，改动不会写回中心"); }).catch((e) => setFile({ error: errorText(e) }))}
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
              <TreeLevel dir="" depth={0} dirs={dirs} open={open} selected={path} onToggle={toggle} onPick={pick} marks={NO_MARKS} />
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

/** What the 文件 pane had open for a scope: switching dock tabs or conversations unmounts it. */
type PaneSaved = { tabs: string[]; active: string | null; open: Set<string>; showAll: boolean };
const remembered = new Map<string, PaneSaved>();

/** Below this width the tree and the file take turns; above it they stand side by side. */
const WIDE_PANE = 660;

/** The pane's own browser: tabs of files, one tree, a workspace-wide name filter and git marks. */
function usePaneBrowser(scope: FileScope, saved?: PaneSaved) {
  const key = scopeKey(scope);
  const [showAll, setShowAll] = useState(saved?.showAll ?? false);
  const [dirs, setDirs] = useState<Map<string, Dir>>(new Map());
  const [open, setOpen] = useState<Set<string>>(saved?.open ?? new Set([""]));
  const [root, setRoot] = useState("");
  const [tabs, setTabs] = useState<string[]>(() =>
    saved ? [...new Set([...saved.tabs, ...(saved.active ? [saved.active] : [])])] : [],
  );
  const [active, setActive] = useState<string | null>(saved?.active ?? null);
  const [line, setLine] = useState<number | undefined>(undefined);
  const [file, setFile] = useState<FileState>(null);
  const [reloads, setReloads] = useState(0);
  const [status, setStatus] = useState<FileStatusEntry[]>([]);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<string[] | null>(null);
  const [truncated, setTruncated] = useState(false);
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState<string>();

  const loadDir = (dir: string, all = showAll) => {
    setDirs((map) => new Map(map).set(dir, map.get(dir) && map.get(dir) !== "loading" ? map.get(dir)! : "loading"));
    api.filesTree(scope, dir, all).then(
      (r) => {
        if (dir === "") setRoot(r.root);
        setDirs((map) => new Map(map).set(dir, r.entries));
      },
      (e) => setDirs((map) => new Map(map).set(dir, { error: errorText(e) })),
    );
  };

  // The tree's root, and everything again when ignored files are shown or hidden.
  useEffect(() => {
    setDirs(new Map());
    for (const dir of open) loadDir(dir, showAll);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showAll, reloads]);

  // The working copy's marks; an older hub or a non-git directory simply has none.
  useEffect(() => {
    let live = true;
    api.filesStatus(scope).then(
      (r) => live && setStatus(r.git ? r.entries : []),
      () => live && setStatus([]),
    );
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, reloads]);

  // The active file.
  useEffect(() => {
    if (!active) {
      setFile(null);
      return;
    }
    let live = true;
    setFile("loading");
    api.filesContent(scope, active).then(
      (r) => live && setFile(r),
      (e) => live && setFile({ error: errorText(e) }),
    );
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, reloads]);

  // Names across the whole workspace, debounced while typing.
  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setResults(null);
      setTruncated(false);
      setSearchError(undefined);
      setSearching(false);
      return;
    }
    let live = true;
    setSearching(true);
    const timer = setTimeout(() => {
      api.filesSearch(scope, q, showAll).then(
        (r) => {
          if (!live) return;
          setResults(r.paths);
          setTruncated(r.truncated);
          setSearchError(undefined);
          setSearching(false);
        },
        (e) => {
          if (!live) return;
          setResults([]);
          setTruncated(false);
          setSearchError(errorText(e));
          setSearching(false);
        },
      );
    }, 180);
    return () => {
      live = false;
      clearTimeout(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, query, showAll, reloads]);

  const toggle = (dir: string) => {
    setOpen((current) => {
      const next = new Set(current);
      if (next.has(dir)) next.delete(dir);
      else {
        next.add(dir);
        if (!dirs.has(dir)) loadDir(dir);
      }
      return next;
    });
  };

  /** Open a file in a tab (or bring its existing tab forward) and reveal it in the tree. */
  const openPath = (path: string, at?: number) => {
    setTabs((current) => (current.includes(path) ? current : [...current, path]));
    setActive(path);
    setLine(at);
    const ups = parentDirs(path);
    setOpen((current) => new Set([...current, "", ...ups]));
    for (const dir of ups) if (!dirs.has(dir)) loadDir(dir);
  };

  /** Back to the tree; the open tabs stay. */
  const clear = () => {
    setActive(null);
    setLine(undefined);
  };

  /** Close one tab; the next one takes its place, or the pane goes back to the tree. */
  const closeTab = (path: string) => {
    const at = tabs.indexOf(path);
    const next = tabs.filter((tab) => tab !== path);
    setTabs(next);
    if (active === path) {
      setActive(next[Math.min(at, next.length - 1)] ?? null);
      setLine(undefined);
    }
  };

  const reload = () => setReloads((n) => n + 1);

  return {
    showAll,
    setShowAll,
    dirs,
    open,
    root,
    tabs,
    active,
    line,
    setLine,
    file,
    reloads,
    status,
    query,
    setQuery,
    results,
    truncated,
    searching,
    searchError,
    toggle,
    openPath,
    clear,
    closeTab,
    reload,
    loadDir,
    setFile,
  };
}

/** The 文件 pane: tabs and content beside the tree when it is wide enough, otherwise in turn. */
export function FilesPane({ scope }: { scope: FileScope }) {
  const key = scopeKey(scope);
  const [saved] = useState(() => remembered.get(key));
  const b = usePaneBrowser(scope, saved);
  const [wide, setWide] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  const [source, setSource] = useState<Record<string, boolean>>({});
  const [outline, setOutline] = useState(true);
  const bridge = getBridge();
  const marks = useMemo(() => changeIndex(b.status), [b.status]);
  const loaded = loadedOf(b.file);
  const markdown = !!loaded && langOf(loaded.path) === "md";
  const headings = useMemo(() => (markdown && loaded?.text ? outlineOf(loaded.text) : []), [markdown, loaded?.text]);
  const sourceOfActive = b.active ? (source[b.active] ?? !!b.line) : false;

  // The panel's own width decides the layout, not the window's: it may sit in either dock.
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const measure = () => setWide(el.getBoundingClientRect().width >= WIDE_PANE);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    remembered.set(key, { tabs: b.tabs, active: b.active, open: b.open, showAll: b.showAll });
  }, [key, b.tabs, b.active, b.open, b.showAll]);

  // A line from a document link is about the source.
  useEffect(() => {
    if (b.active && b.line) setSource((current) => (current[b.active!] ? current : { ...current, [b.active!]: true }));
  }, [b.active, b.line]);

  const refresh = (
    <button className="ghost icon" onClick={b.reload} title="重新读取" aria-label="重新读取">
      <RefreshCw size={14} />
    </button>
  );

  const tree = (
    <FilesTree
      root={b.root}
      showAll={b.showAll}
      onShowAll={b.setShowAll}
      refresh={b.active ? null : refresh}
      dirs={b.dirs}
      open={b.open}
      selected={b.active}
      onToggle={b.toggle}
      onPick={b.openPath}
      marks={marks}
      query={b.query}
      onQuery={b.setQuery}
      results={b.results}
      truncated={b.truncated}
      searching={b.searching}
      searchError={b.searchError}
    />
  );

  const setActiveSource = (value: boolean) => {
    if (b.active) setSource((current) => ({ ...current, [b.active!]: value }));
  };

  const main = b.active ? (
    <div className="fp-main">
      <FileTabs tabs={b.tabs} active={b.active} onPick={b.openPath} onClose={b.closeTab} narrow={!wide} onBack={b.clear} />
      <FileCrumbs
        path={b.active}
        line={b.line}
        title={loaded?.abs_path}
        markdown={markdown}
        source={sourceOfActive}
        onSource={setActiveSource}
        outline={outline}
        onOutline={setOutline}
        canOutline={headings.length > 0}
        onZoom={() => b.active && openFile({ scope, path: b.active, line: b.line })}
        onReload={b.reload}
        canOpen={!!bridge?.openPath && !!loaded}
        onOpen={(path) => bridge?.openPath?.(scopePath(scope), path).then(({ copy }) => { if (copy) window.alert("打开的是副本，改动不会写回中心"); }).catch((e) => b.setFile({ error: errorText(e) }))}
      />
      <section className="fv-code-pane">
        <FileBody
          scope={scope}
          file={b.file}
          line={b.line}
          onLine={b.setLine}
          nonce={b.reloads}
          onFollow={(path, at) => b.openPath(path, at)}
          source={markdown ? sourceOfActive : undefined}
          onSource={markdown ? setActiveSource : undefined}
          outline={outline}
          onOutline={setOutline}
        />
      </section>
    </div>
  ) : (
    <div className="fp-main fp-blank">
      <div className="fv-empty">从{wide ? "右侧" : "文件列表"}打开一个文件。</div>
    </div>
  );

  return (
    <div ref={box} className={`fp ${wide ? "fp-wide" : "fp-narrow"}`}>
      {wide ? (
        <>
          {main}
          <aside className="fp-tree">{tree}</aside>
        </>
      ) : b.active ? (
        main
      ) : (
        tree
      )}
    </div>
  );
}

/** The files open in the pane, with a close button each. */
function FileTabs({ tabs, active, onPick, onClose, narrow, onBack }: { tabs: string[]; active: string | null; onPick: (path: string) => void; onClose: (path: string) => void; narrow: boolean; onBack: () => void }) {
  if (!tabs.length) return null;
  return (
    <div className="fp-tabs">
      {narrow && (
        <button className="ghost icon fp-back" onClick={onBack} title="回到文件列表" aria-label="回到文件列表">
          <ChevronLeft size={15} />
        </button>
      )}
      <div className="fp-tab-scroll" role="tablist" aria-label="打开的文件">
        {tabs.map((path) => (
          <span key={path} className={`fp-tab ${path === active ? "on" : ""}`}>
            <button role="tab" aria-selected={path === active} className="fp-tab-pick" onClick={() => onPick(path)} title={path}>
              <span className="fp-tab-name">{baseOf(path)}</span>
            </button>
            <button className="fp-tab-x" onClick={() => onClose(path)} title={`关闭 ${baseOf(path)}`} aria-label={`关闭 ${baseOf(path)}`}>
              <X size={12} />
            </button>
          </span>
        ))}
      </div>
    </div>
  );
}

/** Path, preview/source and outline switch, and the way back to the big viewer. */
function FileCrumbs({
  path,
  line,
  title,
  markdown,
  source,
  onSource,
  outline,
  onOutline,
  canOutline,
  onZoom,
  onReload,
  canOpen,
  onOpen,
}: {
  path: string;
  line?: number;
  title?: string;
  markdown: boolean;
  source: boolean;
  onSource: (value: boolean) => void;
  outline: boolean;
  onOutline: (value: boolean) => void;
  canOutline: boolean;
  onZoom: () => void;
  onReload: () => void;
  canOpen: boolean;
  onOpen: (path: string) => void;
}) {
  const parts = path.split("/");
  return (
    <div className="fp-crumbs">
      <nav className="fp-crumb" title={title ?? path} aria-label="文件路径">
        {parts.map((part, i) => (
          <span key={i}>
            <span className={i === parts.length - 1 ? "leaf" : ""}>{part}</span>
            {i < parts.length - 1 && <span className="sep">\</span>}
          </span>
        ))}
        {line && <span className="fp-line-tag">第 {line} 行</span>}
      </nav>
      <div className="fp-crumb-actions">
        {markdown && <DisplayControls source={source} onSource={onSource} outline={outline} onOutline={onOutline} canOutline={canOutline} />}
        <button className="ghost icon" onClick={onReload} title="重新读取" aria-label="重新读取">
          <RefreshCw size={13} />
        </button>
        {canOpen && (
          <button className="ghost icon" onClick={() => onOpen(path)} title="用系统默认程序打开这个文件" aria-label="用系统默认程序打开">
            <ExternalLink size={13} />
          </button>
        )}
        <button className="ghost icon" onClick={onZoom} title="在中间弹窗打开当前文件和行" aria-label="放大">
          <Maximize2 size={14} />
        </button>
      </div>
    </div>
  );
}

/** Preview/source and the outline's on/off, shared by the pane and the popup. */
function DisplayControls({ source, onSource, outline, onOutline, canOutline }: { source: boolean; onSource: (value: boolean) => void; outline: boolean; onOutline: (value: boolean) => void; canOutline: boolean }) {
  return (
    <>
      <div className="segmented small" role="tablist" aria-label="Markdown 显示方式">
        <button role="tab" aria-selected={!source} className={!source ? "active" : ""} onClick={() => onSource(false)}>
          预览
        </button>
        <button role="tab" aria-selected={source} className={source ? "active" : ""} onClick={() => onSource(true)}>
          源码
        </button>
      </div>
      {canOutline && (
        <button className={`ghost icon ${outline ? "on" : ""}`} onClick={() => onOutline(!outline)} title={outline ? "隐藏大纲" : "显示大纲"} aria-pressed={outline} aria-label={outline ? "隐藏大纲" : "显示大纲"}>
          <List size={14} />
        </button>
      )}
    </>
  );
}

function FilesTree({
  root,
  showAll,
  onShowAll,
  refresh,
  dirs,
  open,
  selected,
  onToggle,
  onPick,
  marks,
  query,
  onQuery,
  results,
  truncated,
  searching,
  searchError,
}: {
  root: string;
  showAll: boolean;
  onShowAll: (update: (v: boolean) => boolean) => void;
  refresh: React.ReactNode;
  dirs: Map<string, Dir>;
  open: Set<string>;
  selected: string | null;
  onToggle: (dir: string) => void;
  onPick: (path: string) => void;
  marks: ChangeMark;
  query: string;
  onQuery: (value: string) => void;
  results: string[] | null;
  truncated: boolean;
  searching: boolean;
  searchError?: string;
}) {
  const filtered = !!query.trim();
  return (
    <div className="fp-tree-pane">
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
      <div className="fp-filter">
        <Search size={13} />
        <input value={query} onChange={(e) => onQuery(e.target.value)} placeholder="过滤整个工作区…" aria-label="过滤文件" spellCheck={false} />
        {query && (
          <button className="ghost icon" onClick={() => onQuery("")} title="清除过滤" aria-label="清除过滤">
            <X size={12} />
          </button>
        )}
      </div>
      <div className="fv-tree-scroll">
        {filtered ? (
          <SearchResults paths={results ?? []} searching={searching} error={searchError} truncated={truncated} selected={selected} marks={marks} onPick={onPick} />
        ) : (
          <TreeLevel dir="" depth={0} dirs={dirs} open={open} selected={selected} onToggle={onToggle} onPick={onPick} marks={marks} />
        )}
      </div>
    </div>
  );
}

function SearchResults({ paths, searching, error, truncated, selected, marks, onPick }: { paths: string[]; searching: boolean; error?: string; truncated: boolean; selected: string | null; marks: ChangeMark; onPick: (path: string) => void }) {
  if (searching && !paths.length) return <div className="fv-tree-note">搜索中…</div>;
  if (error) return <div className="fv-tree-note err">{error}</div>;
  if (!paths.length) return <div className="fv-tree-note">没有匹配的文件。</div>;
  return (
    <>
      {paths.map((path) => {
        const change = marks.files.get(path);
        const dir = dirOf(path);
        return (
          <button key={path} className={`fv-node search-hit ${selected === path ? "sel" : ""}`} onClick={() => onPick(path)} title={path}>
            <span className="chev" />
            <File size={14} className="ico" />
            <span className="fp-hit-dir">{dir ? `${dir}\\` : ""}</span>
            <span className={`name ${change ? `changed ${change}` : ""}`}>{baseOf(path)}</span>
            {change && (
              <span className={`fv-badge ${change}`} title={change}>
                {changeMark(change)}
              </span>
            )}
          </button>
        );
      })}
      {truncated && <div className="fv-tree-note">结果太多，只显示前 200 个。</div>}
    </>
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
  marks: ChangeMark;
}) {
  const { dir, depth, dirs, open, selected, onToggle, onPick, marks } = props;
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
        const change = isDir ? undefined : marks.files.get(e.path);
        const changedDir = isDir && marks.dirs.has(e.path);
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
              <span className={`name ${change ? `changed ${change}` : ""}`}>{e.name}</span>
              {changedDir && <span className="fv-dot" title="这个文件夹里有未提交的改动" aria-label="这个文件夹里有未提交的改动" />}
              {change && (
                <span className={`fv-badge ${change}`} title={change}>
                  {changeMark(change)}
                </span>
              )}
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
  source,
  onSource,
  outline,
  onOutline,
}: {
  scope: FileScope;
  file: FileState;
  line?: number;
  onLine: (n: number) => void;
  nonce: number;
  /** A link in a rendered document was clicked; the path is from the workspace root. */
  onFollow: (path: string, line?: number) => void;
  /** Controlled from the pane's breadcrumb; omitted in the popup, which shows its own bar. */
  source?: boolean;
  onSource?: (value: boolean) => void;
  outline?: boolean;
  onOutline?: (value: boolean) => void;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const uid = useId().replace(/:/g, "");
  const text = file && file !== "loading" && !("error" in file) && !file.binary ? (file.text ?? "") : null;
  const lang = file && file !== "loading" && !("error" in file) ? langOf(file.path) : "text";
  const lines = useMemo(() => (text === null ? [] : highlight(text, lang)), [text, lang]);
  // A link to a line is about the source; otherwise the person's choice holds from file to file.
  const [choice, setChoice] = useState<boolean | null>(null);
  const [ownOutline, setOwnOutline] = useState(true);
  const controlled = source !== undefined;
  useEffect(() => {
    if (!controlled && line) setChoice(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nonce]);
  const showSource = source ?? choice ?? !!line;
  const rendered = lang === "md" && !showSource;
  const showOutline = outline ?? ownOutline;
  const filePath = loadedOf(file)?.path ?? "";
  const docLinks = { dir: filePath.slice(0, Math.max(0, filePath.lastIndexOf("/"))), open: onFollow };
  const headings = useMemo(() => (lang === "md" && text !== null ? outlineOf(text) : []), [lang, text]);
  const headingIds = useMemo(() => headings.map((heading) => `${uid}-h${heading.index}`), [uid, headings]);
  const setSource = onSource ?? setChoice;
  const setOutline = onOutline ?? setOwnOutline;

  // Bring the wanted line into view once the file is there.
  useEffect(() => {
    if (!line || !lines.length) return;
    const el = scroller.current?.querySelector<HTMLElement>(`[data-line="${line}"]`);
    el?.scrollIntoView({ block: "center" });
  }, [line, lines, nonce, showSource]);

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
      {lang === "md" && !controlled && (
        <div className="fv-md-bar">
          <DisplayControls source={showSource} onSource={(value) => setSource(value)} outline={showOutline} onOutline={(value) => setOutline(value)} canOutline={headings.length > 0} />
        </div>
      )}
      {rendered ? (
        <MarkdownView text={text ?? ""} scope={scope} links={docLinks} headings={headings} headingIds={headingIds} outline={showOutline} />
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

/** Rendered Markdown with the outline rail on its left; used by the pane and the popup. */
function MarkdownView({ text, scope, links, headings, headingIds, outline }: { text: string; scope: FileScope; links: { dir: string; open: (path: string, line?: number) => void }; headings: Heading[]; headingIds: string[]; outline: boolean }) {
  const scroll = useRef<HTMLDivElement>(null);
  const [current, setCurrent] = useState(0);

  // Which heading's section is at the top, updated as the document scrolls.
  useEffect(() => {
    const box = scroll.current;
    if (!box) return;
    let frame = 0;
    const measure = () => {
      frame = 0;
      if (!headings.length) return;
      const top = box.getBoundingClientRect().top;
      const tops = headings.map((heading) => {
        const el = box.querySelector<HTMLElement>(`[data-heading="${heading.index}"]`);
        return el ? el.getBoundingClientRect().top - top + box.scrollTop : Number.POSITIVE_INFINITY;
      });
      setCurrent(Math.max(0, currentHeading(tops, box.scrollTop + 8)));
    };
    const onScroll = () => {
      if (!frame) frame = requestAnimationFrame(measure);
    };
    box.addEventListener("scroll", onScroll, { passive: true });
    frame = requestAnimationFrame(measure);
    return () => {
      box.removeEventListener("scroll", onScroll);
      if (frame) cancelAnimationFrame(frame);
    };
  }, [headings, text, outline]);

  const jump = (index: number) => {
    const box = scroll.current;
    const el = box?.querySelector<HTMLElement>(`[data-heading="${index}"]`);
    if (!box || !el) return;
    const top = el.getBoundingClientRect().top - box.getBoundingClientRect().top + box.scrollTop - 8;
    box.scrollTo({ top, behavior: "smooth" });
  };

  return (
    <div className="md-view">
      {outline && headings.length > 0 && <Outline headings={headings} current={current} onJump={jump} />}
      <div className="fv-md" ref={scroll}>
        <FileScopeProvider value={scope}>
          <DocLinksProvider value={links}>
            <Prose doc text={text} headingIds={headingIds} />
          </DocLinksProvider>
        </FileScopeProvider>
      </div>
    </div>
  );
}

/** A line per heading at the left edge; hovering opens the full list, the current one lit. */
function Outline({ headings, current, onJump }: { headings: Heading[]; current: number; onJump: (index: number) => void }) {
  return (
    <div className="md-outline" aria-label="文档大纲">
      <div className="md-outline-lines">
        {headings.map((heading) => (
          <button
            key={heading.index}
            className={`md-ol-line lvl-${heading.level} ${heading.index === current ? "on" : ""}`}
            onClick={() => onJump(heading.index)}
            title={heading.text}
            aria-label={`跳到 ${heading.text}`}
          >
            <span className="md-ol-bar" />
          </button>
        ))}
      </div>
      <div className="md-outline-pop" aria-label="标题列表">
        {headings.map((heading) => (
          <button
            key={heading.index}
            className={`md-ol-item lvl-${heading.level} ${heading.index === current ? "on" : ""}`}
            onClick={() => onJump(heading.index)}
            title={heading.text}
          >
            {heading.text}
          </button>
        ))}
      </div>
    </div>
  );
}
