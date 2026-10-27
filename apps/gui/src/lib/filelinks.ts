// Which link targets and inline code in a reply name a local file, and the line they point at.
// Pure; tested in filelinks.test.ts. Whether the file exists is the hub's to say
// (`files/resolve`): a link is shown as a file link at once (the person wrote it as a link),
// inline code only once the hub confirmed it.

/** A file a reply points at: the path as written (relative or absolute), and a line. */
export type FileTarget = { path: string; line?: number; col?: number };

/** `:12`, `:12:5`, `#L12`, `#L12C5`, `#L12-L20` at the end of a path. */
const LINE_SUFFIX = /(?::(\d+)(?::(\d+))?|#L(\d+)(?:C(\d+))?(?:-L?\d+(?:C\d+)?)?)$/;
/** `C:\` or `C:/`: a Windows drive, not a scheme. */
const DRIVE = /^[a-zA-Z]:[\\/]/;
/** `javascript:`, `file:`, `vscode:`, `data:`… */
const SCHEME = /^[a-zA-Z][a-zA-Z0-9+.-]*:/;
/** What no path a reply names contains (and what HTML or a shell would read as markup). */
const NOT_IN_PATH = /[\s<>"'`|*?{}()[\]^,;=!$&@]/;

/** The path and line of `s` (`src/main.rs:12:5` → `src/main.rs`, 12, 5). */
export function splitLine(s: string): FileTarget {
  const m = s.match(LINE_SUFFIX);
  if (!m || m.index === undefined || m.index === 0) return { path: s };
  const line = Number(m[1] ?? m[3]);
  const colText = m[2] ?? m[4];
  const out: FileTarget = { path: s.slice(0, m.index) };
  if (line > 0) out.line = line;
  if (colText !== undefined && Number(colText) > 0) out.col = Number(colText);
  return out;
}

/** Whether `p` is an absolute path: `C:\x`, `C:/x` or `/x`. */
export function isAbsolutePath(p: string): boolean {
  return DRIVE.test(p) || (p.startsWith("/") && !p.startsWith("//"));
}

function decode(s: string): string {
  try {
    return decodeURI(s);
  } catch {
    return s;
  }
}

/** The shape every path we link must have, after its line suffix is gone. */
function plausiblePath(p: string): boolean {
  if (!p || p.length > 400) return false;
  if (p.startsWith("//") || p.startsWith("\\\\")) return false; // a host or a UNC share
  if (p.startsWith("~") || p.startsWith("-") || p.startsWith("#") || p.startsWith("?")) return false;
  if (!DRIVE.test(p) && SCHEME.test(p)) return false;
  if (/[\u0000-\u001f]/.test(p)) return false;
  // Only `.`, `/` and `\`: no file in it.
  if (/^[./\\]+$/.test(p)) return false;
  return true;
}

/**
 * The file a Markdown link target names, or undefined when it is not a local path: http(s) and
 * mailto are web links, any other scheme (`javascript:`, `file:`…) is refused, and so is "".
 */
export function fileLinkTarget(href: string): FileTarget | undefined {
  const raw = href.trim();
  if (!raw) return undefined;
  if (/^(https?:|mailto:)/i.test(raw)) return undefined;
  const text = decode(raw);
  const target = splitLine(text);
  // `main.rs:12` reads like a scheme until its line is gone; anything left with one is refused.
  if (!plausiblePath(target.path)) return undefined;
  // `javascript:1` would split into `javascript` and a line: a scheme-looking word with only a
  // number after it is a file only when it looks like one.
  if (!DRIVE.test(text) && SCHEME.test(text) && !/[./\\]/.test(target.path)) return undefined;
  if (/[<>"|*?\s]/.test(target.path)) return undefined;
  return target;
}

/** A file extension: `.rs`, `.tsx`, `.d.ts`; not `1.2` nor a sentence's `.` */
const EXTENSION = /\.[a-zA-Z][a-zA-Z0-9]{0,9}$/;

/**
 * Whether inline code reads like a file path worth asking the hub about: no spaces, and a `/`
 * or a file extension (`src/lib.rs`, `Cargo.toml`, `docs/`, `main.rs:12`). Not a URL, a flag or
 * a call (`foo()`). The hub still has to confirm it exists before it becomes a link.
 */
export function codeFileTarget(code: string): FileTarget | undefined {
  const text = code.trim();
  if (!text || /\s/.test(text)) return undefined;
  const target = splitLine(text);
  const p = target.path;
  if (!plausiblePath(p)) return undefined;
  if (NOT_IN_PATH.test(p)) return undefined;
  if (/^[\d.]+$/.test(p)) return undefined;
  const hasSlash = /[^/\\][/\\]|^[/\\][^/\\]|[/\\]$/.test(p);
  const name = p.replace(/[/\\]+$/, "").split(/[/\\]/).pop() ?? "";
  const hasExtension = EXTENSION.test(name) && !/^\.+$/.test(name);
  if (!hasSlash && !hasExtension) return undefined;
  // `/v1/agents`-style API routes and the like resolve to nothing; the hub says so.
  return target;
}
