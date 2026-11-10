// The pure parts of the built-in browser's CDP driver (electron/browserDriver.cts): the
// accessibility tree as indented text with [ref=N] marks, the key names `press` takes, the
// console entries and the centre of a box. Kept apart from Electron so a test can run them.

/** One node of `Accessibility.getFullAXTree`, the fields the snapshot reads. */
export type AxNode = {
  nodeId: string;
  ignored?: boolean;
  role?: AxValue;
  name?: AxValue;
  value?: AxValue;
  childIds?: string[];
  backendDOMNodeId?: number;
};

type AxValue = { type?: string; value?: unknown };

/** Roles a model can act on: each gets a `[ref=N]` when a DOM node stands behind it. */
export const INTERACTIVE_ROLES: ReadonlySet<string> = new Set([
  "button", "link", "textbox", "searchbox", "combobox", "checkbox", "radio", "switch", "slider",
  "spinbutton", "tab", "menuitem", "menuitemcheckbox", "menuitemradio", "option", "listbox", "treeitem",
]);

/** Roles that only group: their children are printed in their place. */
const TRANSPARENT_ROLES: ReadonlySet<string> = new Set([
  "none", "generic", "presentation", "GenericContainer", "RootWebArea", "InlineTextBox", "LineBreak",
]);

/** Text leaves whose words the parent's name already carries: not printed again under it. */
const TEXT_ROLES: ReadonlySet<string> = new Set(["StaticText", "InlineTextBox"]);

/** Enough for any page a model reads; more is said to be cut. */
export const MAX_SNAPSHOT_LINES = 2000;

function valueText(v: AxValue | undefined): string {
  const raw = v?.value;
  if (raw === undefined || raw === null) return "";
  return String(raw).replace(/\s+/g, " ").trim();
}

export type Snapshot = {
  /** Indented lines: `- role "name": value [ref=N]`. */
  text: string;
  /** ref → backendDOMNodeId. Rebuilt by every snapshot; refs from an older one are gone. */
  refs: Map<number, number>;
  truncated: boolean;
};

/**
 * Turns the accessibility tree into text. Ignored and grouping nodes disappear and their
 * children move up a level; a node with no name and no value that a model cannot act on is
 * skipped too. Refs count from 1 in document order.
 */
export function axSnapshot(nodes: readonly AxNode[]): Snapshot {
  const byId = new Map(nodes.map((n) => [n.nodeId, n] as const));
  const refs = new Map<number, number>();
  const lines: string[] = [];
  const seen = new Set<string>();
  let truncated = false;

  const walk = (id: string, depth: number): void => {
    if (seen.has(id)) return;
    seen.add(id);
    const node = byId.get(id);
    if (!node) return;
    const role = valueText(node.role);
    const name = valueText(node.name);
    const value = valueText(node.value);
    const children = node.childIds ?? [];
    const interactive = INTERACTIVE_ROLES.has(role);
    const printable =
      !node.ignored && role !== "" && !TRANSPARENT_ROLES.has(role) && (name !== "" || value !== "" || interactive);
    if (!printable) {
      for (const child of children) walk(child, depth);
      return;
    }
    if (lines.length >= MAX_SNAPSHOT_LINES) {
      truncated = true;
      return;
    }
    let ref = "";
    if (interactive && typeof node.backendDOMNodeId === "number") {
      const n = refs.size + 1;
      refs.set(n, node.backendDOMNodeId);
      ref = ` [ref=${n}]`;
    }
    const label = role === "StaticText" ? JSON.stringify(name) : `${role}${name ? ` ${JSON.stringify(name)}` : ""}`;
    const shown = value !== "" && role !== "StaticText" ? `: ${JSON.stringify(value)}` : "";
    lines.push(`${"  ".repeat(depth)}- ${label}${shown}${ref}`);
    for (const child of children) {
      const childNode = byId.get(child);
      if (name !== "" && childNode && TEXT_ROLES.has(valueText(childNode.role))) continue;
      walk(child, depth + 1);
    }
  };

  if (nodes.length > 0) walk(nodes[0].nodeId, 0);
  if (truncated) lines.push(`… 超过 ${MAX_SNAPSHOT_LINES} 行，后面的内容没有列出`);
  return { text: lines.join("\n"), refs, truncated };
}

/** The DOM node behind a ref from the latest snapshot; throws when there is none. */
export function backendNodeFor(refs: ReadonlyMap<number, number>, ref: number): number {
  const id = refs.get(ref);
  if (id === undefined) throw new Error(`没有 ref=${ref}：请先 browser_snapshot 取得最新的 ref`);
  return id;
}

/** The centre of a box given as `DOM.getBoxModel`'s content quad (four x,y corners). */
export function quadCenter(quad: readonly number[]): { x: number; y: number } {
  if (quad.length !== 8 || quad.some((n) => !Number.isFinite(n))) throw new Error("元素没有可点击的区域");
  return {
    x: (quad[0] + quad[2] + quad[4] + quad[6]) / 4,
    y: (quad[1] + quad[3] + quad[5] + quad[7]) / 4,
  };
}

export type KeyDef = { key: string; code: string; keyCode: number; text?: string };

const NAMED: ReadonlyMap<string, KeyDef> = new Map<string, KeyDef>([
  ["Enter", { key: "Enter", code: "Enter", keyCode: 13, text: "\r" }],
  ["Tab", { key: "Tab", code: "Tab", keyCode: 9 }],
  ["Escape", { key: "Escape", code: "Escape", keyCode: 27 }],
  ["Backspace", { key: "Backspace", code: "Backspace", keyCode: 8 }],
  ["Delete", { key: "Delete", code: "Delete", keyCode: 46 }],
  ["Space", { key: " ", code: "Space", keyCode: 32, text: " " }],
  ["ArrowUp", { key: "ArrowUp", code: "ArrowUp", keyCode: 38 }],
  ["ArrowDown", { key: "ArrowDown", code: "ArrowDown", keyCode: 40 }],
  ["ArrowLeft", { key: "ArrowLeft", code: "ArrowLeft", keyCode: 37 }],
  ["ArrowRight", { key: "ArrowRight", code: "ArrowRight", keyCode: 39 }],
  ["Home", { key: "Home", code: "Home", keyCode: 36 }],
  ["End", { key: "End", code: "End", keyCode: 35 }],
  ["PageUp", { key: "PageUp", code: "PageUp", keyCode: 33 }],
  ["PageDown", { key: "PageDown", code: "PageDown", keyCode: 34 }],
]);

/**
 * What a CDP key name means: the key, its code, its Windows virtual-key code and the text it
 * types. Named keys by their CDP names; a single letter or digit by itself. Anything else is
 * undefined, so an unknown name is refused rather than sent as nothing.
 */
export function keyDefinition(name: string): KeyDef | undefined {
  const named = NAMED.get(name);
  if (named) return named;
  if (/^[A-Za-z]$/.test(name)) {
    return { key: name, code: `Key${name.toUpperCase()}`, keyCode: name.toUpperCase().charCodeAt(0), text: name };
  }
  if (/^[0-9]$/.test(name)) {
    return { key: name, code: `Digit${name}`, keyCode: 48 + Number(name), text: name };
  }
  return undefined;
}

export type ConsoleEntry = { level: string; text: string; ts: number };

function argText(arg: { type?: string; value?: unknown; unserializableValue?: string; description?: string }): string {
  if (arg.value !== undefined) return typeof arg.value === "string" ? arg.value : JSON.stringify(arg.value);
  return arg.unserializableValue ?? arg.description ?? arg.type ?? "";
}

/**
 * A console entry from a CDP event (`Runtime.consoleAPICalled`, `Runtime.exceptionThrown`);
 * undefined for any other event.
 */
export function consoleEntry(method: string, params: unknown): ConsoleEntry | undefined {
  const p = (params ?? {}) as Record<string, unknown>;
  if (method === "Runtime.consoleAPICalled") {
    const args = (Array.isArray(p.args) ? p.args : []) as Parameters<typeof argText>[0][];
    return { level: String(p.type ?? "log"), text: args.map(argText).join(" "), ts: Number(p.timestamp ?? 0) };
  }
  if (method === "Runtime.exceptionThrown") {
    const details = (p.exceptionDetails ?? {}) as { text?: string; exception?: { description?: string } };
    return {
      level: "error",
      text: details.exception?.description ?? details.text ?? "未捕获的异常",
      ts: Number(p.timestamp ?? 0),
    };
  }
  return undefined;
}
