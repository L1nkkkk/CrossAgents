// The built-in browser (run note browser/contract §3): every tab is a `<webview>` on the
// `persist:browser` session, with an address bar and its page's title above it. Tabs are dock
// tabs like terminals (`web:<id>`, panes.ts) and their pages are kept out of React, so moving one
// between the docks keeps its page; their urls are saved with the layout, so a reload brings the
// tabs back at the same pages.
//
// Agents drive the same tabs (§2): the hub sends `browser.request`, this page claims it, runs the
// tool against the tab and posts the result. The person can take a tab over meanwhile; while it
// is theirs, a request for it is answered `human_control` until they hand it back.

import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { api, errorText, getBridge, scopeKey, type FileScope } from "./api";
import type { BrowserBridge } from "./bridge";
import { Icon } from "./components";
import { resolvePath } from "./fileResolve";
import { onHubEvent } from "./hub";
import { dockOf, type DockId } from "./lib/docks";
import { docksNow, isWebTab, placeTab, rememberWebUrl, shutTab, useLayout, webTab, webUrl, type WebTab } from "./panes";
import "./browser.css";

/** What the person was told when they took a tab over: the agent's own tools get this back. */
export const HUMAN_CONTROL = "human_control: 这个标签正由人接管，等人点「交还」后再试";

/** Who is driving a tab now: an agent's tool, or the person who took it over. */
type Controller = { kind: "agent"; agent: string } | { kind: "human" };

/** A `<webview>` as Electron adds it to HTMLElement (only the calls this page makes). */
type WebviewTag = HTMLElement & {
  getWebContentsId(): number;
  loadURL(url: string): Promise<void>;
  goBack(): void;
  goForward(): void;
  reload(): void;
  stop(): void;
  canGoBack(): boolean;
  canGoForward(): boolean;
};

/** A navigation event of a `<webview>`. */
type NavEvent = Event & { url?: string; title?: string; errorCode?: number; errorDescription?: string; isMainFrame?: boolean };

/** One browser tab's page. Kept outside React; `publish` is what re-renders the docks. */
type Page = {
  /** The `<id>` of `web:<id>`, which is also the tab an agent's tool names. */
  id: string;
  tab: WebTab;
  /** The webContents id the main process knows the tab by; null until its page attached. */
  wcId: number | null;
  url: string;
  /** As the page reports it; `pageName` falls back to the host. */
  title: string;
  loading: boolean;
  back: boolean;
  forward: boolean;
  /** Why the page did not load, until another navigation. */
  problem: string | null;
  controller: Controller | null;
  /** The `<webview>`; null in the mock preview, which has no real page. */
  el: WebviewTag | null;
  created: number;
  /** A fresh tab puts the cursor in the address bar once it shows. */
  focusBar: boolean;
};

const pages = new Map<string, Page>();
const listeners = new Set<() => void>();
let view: Page[] = [];
let created = 0;

function publish() {
  view = [...pages.values()].sort((a, b) => a.created - b.created);
  listeners.forEach((l) => l());
}

const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => listeners.delete(l);
};

const usePages = () => useSyncExternalStore(subscribe, () => view);

/** Whether this page can open a built-in browser at all. */
export const browserAvailable = () => !!getBridge()?.browser;

/** Whether a `<webview>` exists here: Electron has one, a plain page (the mock preview) does not. */
function supportsWebview(): boolean {
  return typeof (document.createElement("webview") as Partial<WebviewTag>).getWebContentsId === "function";
}

/** The mock preview's own browser (src/dev/browser.mock.ts), for the parts a `<webview>` would do. */
type MockBrowser = BrowserBridge & {
  attach(tab: WebTab, url: string): number;
  page(tab: WebTab): { url: string; title: string; lines: string[] } | undefined;
  navigate(tab: WebTab, action: "back" | "forward" | "reload" | { url: string }): void;
  close(tab: WebTab): void;
};

const mockBrowser = (): MockBrowser | undefined => {
  const browser = getBridge()?.browser as (BrowserBridge & Partial<MockBrowser>) | undefined;
  return browser && typeof browser.attach === "function" ? (browser as MockBrowser) : undefined;
};

/** What the tab is called: its page's title, else its host. */
export function pageName(page: Page): string {
  const title = page.title.trim();
  if (title) return title;
  if (!page.url || page.url === "about:blank") return "新标签页";
  try {
    const url = new URL(page.url);
    if (url.protocol === "crossagents-file:") return decodeURIComponent(url.pathname.split("/").pop() || url.host);
    return url.host || url.pathname;
  } catch {
    return page.url;
  }
}

/** What the address bar opens: as typed when it names a scheme, else over `https://`. */
export function browserUrl(text: string): string | null {
  const trimmed = text.trim();
  if (!trimmed) return null;
  return /^(https?|about|crossagents-file|file):/i.test(trimmed) ? trimmed : `https://${trimmed}`;
}

/** Where a tab's `<webview>` waits while its tab is in another dock: removing it would close the page. */
function vault(): HTMLElement {
  const known = document.getElementById("web-vault");
  if (known) return known;
  const el = document.createElement("div");
  el.id = "web-vault";
  el.hidden = true;
  document.body.appendChild(el);
  return el;
}

function onUrl(page: Page, url: string | undefined) {
  if (!url || url === page.url) return;
  page.url = url;
  rememberWebUrl(page.tab, url);
  publish();
}

/** A tab's page, with its events. `src` is set before it is attached, so it starts loading then. */
function makeWebview(page: Page, url: string): WebviewTag {
  const el = document.createElement("webview") as WebviewTag;
  el.setAttribute("partition", "persist:browser");
  el.setAttribute("data-crossagents-browser", "1");
  el.setAttribute("src", url);
  el.addEventListener("dom-ready", () => {
    page.wcId = el.getWebContentsId();
    page.back = el.canGoBack();
    page.forward = el.canGoForward();
    publish();
  });
  el.addEventListener("did-start-loading", () => {
    page.loading = true;
    page.problem = null;
    publish();
  });
  el.addEventListener("did-stop-loading", () => {
    page.loading = false;
    page.back = el.canGoBack();
    page.forward = el.canGoForward();
    publish();
  });
  el.addEventListener("did-navigate", (e) => onUrl(page, (e as NavEvent).url));
  el.addEventListener("did-navigate-in-page", (e) => onUrl(page, (e as NavEvent).url));
  el.addEventListener("page-title-updated", (e) => {
    const title = (e as NavEvent).title;
    if (typeof title !== "string" || title === page.title) return;
    page.title = title;
    publish();
  });
  el.addEventListener("did-fail-load", (e) => {
    const failure = e as NavEvent;
    if (failure.isMainFrame === false || failure.errorCode === -3) return; // -3: another navigation took over
    page.loading = false;
    page.problem = `打不开这个页面：${failure.errorDescription || "加载失败"}${failure.errorCode ? `（${failure.errorCode}）` : ""}`;
    publish();
  });
  return el;
}

function createPage(id: string, url: string, title?: string): Page {
  const page: Page = {
    id,
    tab: webTab(id),
    wcId: null,
    url,
    title: title ?? "",
    loading: !!url && url !== "about:blank",
    back: false,
    forward: false,
    problem: null,
    controller: null,
    el: null,
    created: ++created,
    focusBar: false,
  };
  pages.set(id, page);
  const mock = mockBrowser();
  if (mock) {
    page.wcId = mock.attach(page.tab, url);
    const shown = mock.page(page.tab);
    if (shown) {
      page.url = shown.url;
      page.title = shown.title;
    }
    page.loading = false;
  } else if (supportsWebview()) {
    page.el = makeWebview(page, url);
  }
  rememberWebUrl(page.tab, page.url);
  publish();
  return page;
}

function dispose(page: Page) {
  page.el?.remove();
  pages.delete(page.id);
  mockBrowser()?.close(page.tab);
}

/** Adopts a tab a saved layout names, at the url saved with it. */
function adopt(tab: WebTab, url: string): Page {
  const id = tab.slice("web:".length);
  const known = pages.get(id);
  if (known) return known;
  return createPage(id, url);
}

/** The dock a new tab goes in: where a browser tab already is, else the right dock. */
function homeDock(): DockId {
  const docks = docksNow();
  const known = [...docks.right.tabs, ...docks.bottom.tabs].find(isWebTab);
  return (known && dockOf(docks, known)) || "right";
}

/** Opens a page in `dock`; no url is a blank tab waiting for the address bar. */
export function openBrowserTab(opts: { url?: string; dock?: DockId; title?: string; focusBar?: boolean } = {}): Page {
  const page = createPage(crypto.randomUUID().slice(0, 8), opts.url ?? "about:blank", opts.title);
  page.focusBar = !!opts.focusBar;
  placeTab(page.tab, opts.dock ?? homeDock());
  return page;
}

/** The + menu's 新建浏览器标签: a blank tab in this dock, the cursor in its address bar. */
export const newBrowserTab = (dock: DockId) => openBrowserTab({ dock, focusBar: true });

/** A tab asked for from outside (another tab's window.open, a local file): opens it at once. */
export const openBrowserUrl = (url: string, dock?: DockId) => openBrowserTab({ url: browserUrl(url) ?? url, dock });

/** Ends a tab: the × on its tab. Its page goes with it. */
export function closeBrowserTab(tab: WebTab) {
  const page = pages.get(tab.slice("web:".length));
  if (page) dispose(page);
  shutTab(tab);
  publish();
}

const roots = new Map<string, string>();

/** The scope's absolute root; asked once per scope. */
async function rootOf(scope: FileScope): Promise<string> {
  const key = scopeKey(scope);
  const known = roots.get(key);
  if (known) return known;
  const tree = await api.filesTree(scope, "", false);
  roots.set(key, tree.root);
  return tree.root;
}

/** Whether opening `path` means showing a page rather than its source. */
export const isHtmlPath = (path: string) => /\.html?$/i.test(path.trim());

/**
 * Opens a local `.html` file in a browser tab, through `browser.fileUrl`. Rejects (with the
 * reason) when the file is not there or the window has no browser, so the caller can show the
 * viewer instead.
 */
export async function openBrowserFile(scope: FileScope, path: string, dock?: DockId) {
  const browser = getBridge()?.browser;
  if (!browser) throw new Error("这个窗口没有内置浏览器");
  const rel = await resolvePath(scope, path);
  if (!rel) throw new Error("文件不存在，或不在这个工作区内");
  const root = (await rootOf(scope)).replace(/[\\/]+$/, "");
  const url = await browser.fileUrl(`${root}/${rel}`);
  openBrowserTab({ url, dock, title: rel.split("/").pop() });
}

// ---- Navigation and control ---------------------------------------------------------------

function navigate(page: Page, text: string) {
  const url = browserUrl(text);
  if (!url) return;
  page.problem = null;
  page.url = url;
  page.loading = url !== "about:blank";
  rememberWebUrl(page.tab, url);
  if (page.el) {
    // A tab that was never shown has no page yet: it starts when the element attaches.
    if (!page.el.isConnected) page.el.setAttribute("src", url);
    else
      void page.el.loadURL(url).catch((err: unknown) => {
        page.problem = `打不开这个页面：${errorText(err)}`;
        page.loading = false;
        publish();
      });
  } else {
    const mock = mockBrowser();
    mock?.navigate(page.tab, { url });
    const shown = mock?.page(page.tab);
    if (shown) {
      page.url = shown.url;
      page.title = shown.title;
    }
    page.loading = false;
    rememberWebUrl(page.tab, page.url);
  }
  publish();
}

/** 后退 / 前进 / 刷新: the tab's own page does it, not the main process. */
function step(page: Page, action: "back" | "forward" | "reload") {
  if (page.el) {
    if (action === "back") page.el.goBack();
    else if (action === "forward") page.el.goForward();
    else page.el.reload();
    return;
  }
  mockBrowser()?.navigate(page.tab, action);
  const shown = mockBrowser()?.page(page.tab);
  if (action === "reload" && shown) {
    page.title = shown.title;
    page.url = shown.url;
  }
  publish();
}

function stop(page: Page) {
  if (page.el) page.el.stop();
  page.loading = false;
  publish();
}

const takeOver = (page: Page) => {
  page.controller = { kind: "human" };
  publish();
};

const handBack = (page: Page) => {
  page.controller = null;
  publish();
};

/** A tab's name for its dock tab. */
export function useBrowserLabel(): (tab: WebTab) => string {
  const all = usePages();
  return (tab) => {
    const page = all.find((p) => p.tab === tab);
    return page ? pageName(page) : "网页";
  };
}

const usePage = (tab: WebTab) => usePages().find((p) => p.tab === tab);

// ---- Agent requests (browser/contract §2) -------------------------------------------------

/** One agent tool call waiting for this page. */
type Request = { id: string; tool: string; input: Record<string, unknown>; agent: string };

/**
 * The hub's `browser.request`, read from the raw event: `@bindings/Event` lists it once the
 * hub's half lands, and this keeps working either way.
 */
function requestOf(event: unknown): Request | null {
  const e = event as { type?: unknown; data?: unknown };
  if (e?.type !== "browser.request" || !e.data) return null;
  const d = e.data as { id?: unknown; tool?: unknown; input?: unknown; agent?: { name?: unknown } };
  if (typeof d.id !== "string" || typeof d.tool !== "string") return null;
  return {
    id: d.id,
    tool: d.tool,
    input: d.input && typeof d.input === "object" ? (d.input as Record<string, unknown>) : {},
    agent: typeof d.agent?.name === "string" ? d.agent.name : "agent",
  };
}

const claimRequest = (id: string) => api.browserClaim(id);
const postResult = (id: string, result: { ok: boolean; output?: unknown; error?: string }) => api.browserResult(id, result);

/** The tab an input names, by either `web:<id>` or the `<id>` itself. */
function pageOf(value: unknown): Page | undefined {
  if (typeof value !== "string" || !value) return undefined;
  return pages.get(value.replace(/^web:/, ""));
}

const tabInfo = (page: Page) => ({
  tab: page.id,
  url: page.url,
  title: pageName(page),
  controlled_by_human: page.controller?.kind === "human",
});

function mustPage(value: unknown, tool: string): Page {
  const page = pageOf(value);
  if (page) return page;
  const known = [...pages.values()].map((p) => p.id).join("、");
  throw new Error(`${tool} 要一个已有的 tab（${value ? `没有 ${String(value)}` : "没给"}）；现在的标签：${known || "没有"}`);
}

function refOf(value: unknown): number {
  const ref = Number(value);
  if (!Number.isInteger(ref) || ref < 1) throw new Error("ref 要是正整数（先 browser_snapshot 拿 ref）");
  return ref;
}

/** Runs one agent tool against a tab; `control` puts the「正在操作」bar on a tab it opens. */
async function runTool(tool: string, input: Record<string, unknown>, control: (page: Page) => void): Promise<unknown> {
  const browser = getBridge()?.browser;
  if (!browser) throw new Error("这个窗口没有内置浏览器");
  if (tool === "browser_tabs") return [...pages.values()].map(tabInfo);
  if (tool === "browser_open") {
    const wanted =
      typeof input.url === "string" && input.url
        ? input.url
        : typeof input.path === "string" && input.path
          ? await browser.fileUrl(input.path)
          : "";
    if (!wanted) throw new Error("browser_open 要给 url 或 path");
    // A named tab is opened there; no tab is a new one.
    if (typeof input.tab === "string" && input.tab) {
      const known = mustPage(input.tab, "browser_open");
      navigate(known, wanted);
      return { tab: known.id, url: browserUrl(wanted) ?? wanted, title: pageName(known) };
    }
    const page = openBrowserTab({ url: browserUrl(wanted) ?? wanted });
    control(page);
    return { tab: page.id, url: page.url, title: pageName(page) };
  }
  const page = mustPage(input.tab, tool);
  const wcId = page.wcId;
  if (wcId === null) throw new Error("这个标签还没加载好，等一下再试");
  switch (tool) {
    case "browser_snapshot":
      return browser.snapshot(wcId);
    case "browser_click":
      await browser.click(wcId, refOf(input.ref));
      return { ok: true };
    case "browser_type": {
      const text = typeof input.text === "string" ? input.text : String(input.text ?? "");
      await browser.type(wcId, refOf(input.ref), text, input.submit === true);
      return { ok: true };
    }
    case "browser_press": {
      if (typeof input.key !== "string" || !input.key) throw new Error("browser_press 要给 key");
      await browser.press(wcId, input.key);
      return { ok: true };
    }
    case "browser_scroll": {
      const dy = Number(input.dy);
      if (!Number.isFinite(dy)) throw new Error("browser_scroll 的 dy 要是数字");
      await browser.scroll(wcId, dy);
      return { ok: true };
    }
    case "browser_screenshot": {
      const shot = await browser.screenshot(wcId);
      return { image_base64: shot.base64, mime: shot.mime };
    }
    case "browser_console":
      return browser.console(wcId);
    case "browser_navigate": {
      const action = input.action;
      if (action !== "back" && action !== "forward" && action !== "reload") throw new Error("browser_navigate 的 action 是 back、forward 或 reload");
      step(page, action);
      return { ok: true, action };
    }
    default:
      throw new Error(`不认识的浏览器工具：${tool}`);
  }
}

/** Claims a request and runs it; a 409 means another window has it, and it is left alone. */
async function serve(request: Request) {
  try {
    await claimRequest(request.id);
  } catch {
    // 409: another window claimed it. Anything else: the hub could not be reached and times
    // the agent out itself.
    return;
  }
  const target = pageOf(request.input.tab);
  if (target?.controller?.kind === "human") {
    await postResult(request.id, { ok: false, error: HUMAN_CONTROL }).catch(() => undefined);
    return;
  }
  const controlled = new Set<Page>();
  const take = (page: Page) => {
    page.controller = { kind: "agent", agent: request.agent };
    controlled.add(page);
  };
  if (target) take(target);
  publish();
  let result: { ok: boolean; output?: unknown; error?: string };
  try {
    result = { ok: true, output: await runTool(request.tool, request.input, take) };
  } catch (err) {
    result = { ok: false, error: errorText(err) };
  }
  // The bar goes when the tool does, unless the person took the tab over meanwhile.
  for (const page of controlled) {
    if (page.controller?.kind === "agent" && page.controller.agent === request.agent) page.controller = null;
  }
  publish();
  await postResult(request.id, result).catch(() => undefined);
}

/**
 * Runs the browser's page half: the saved tabs come back at their pages, the hub's
 * `browser.request` events are claimed and run, and a tab's `window.open` opens a new tab.
 */
export function BrowserHost() {
  const { state } = useLayout();
  const docks = state.docks;
  const hydrated = useRef(false);

  useEffect(() => {
    if (hydrated.current) return;
    hydrated.current = true;
    for (const tab of [...docks.right.tabs, ...docks.bottom.tabs]) {
      if (!isWebTab(tab)) continue;
      // A tab the layout names but no url was saved for never had a page to come back to.
      const url = webUrl(tab);
      if (!url || !browserAvailable()) shutTab(tab);
      else adopt(tab, url);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // A tab closed from anywhere else frees its page.
  useEffect(() => {
    const live = new Set<string>([...docks.right.tabs, ...docks.bottom.tabs]);
    for (const page of [...pages.values()]) if (!live.has(page.tab)) dispose(page);
  }, [docks]);

  useEffect(() => {
    if (!browserAvailable()) return;
    const offRequest = onHubEvent((event) => {
      const request = requestOf(event);
      if (request) void serve(request);
    });
    const offOpen = getBridge()!.browser!.onOpenTab((url) => {
      openBrowserUrl(url);
    });
    return () => {
      offRequest();
      offOpen();
    };
  }, []);

  return null;
}

// ---- What a tab shows ---------------------------------------------------------------------

/** A browser tab's body: address bar, the person/agent line, and the page. */
export function BrowserBody({ tab }: { tab: WebTab }) {
  const page = usePage(tab);
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = page?.el;
    const host = box.current;
    if (!el || !host) return;
    host.appendChild(el);
    // Not removed: a `<webview>` taken out of the page closes it, so it waits in the vault.
    return () => {
      vault().appendChild(el);
    };
  }, [page?.el]);

  if (!page) return <p className="muted small tool-blank">这个浏览器标签已经关掉了。</p>;
  return (
    <div className="web-pane">
      <BrowserBar page={page} />
      <ControlBar page={page} />
      {page.problem && (
        <p className="web-problem" role="alert">
          {page.problem}
        </p>
      )}
      <div className="web-box" ref={box}>
        {!page.el && <MockPage page={page} />}
      </div>
    </div>
  );
}

function BrowserBar({ page }: { page: Page }) {
  const [draft, setDraft] = useState(page.url === "about:blank" ? "" : page.url);
  const [typing, setTyping] = useState(false);
  useEffect(() => {
    if (!typing) setDraft(page.url === "about:blank" ? "" : page.url);
  }, [page.url, typing]);
  return (
    <div className="web-bar">
      <button type="button" className="ghost icon" disabled={!page.back} title={page.back ? "后退" : "没有上一页"} aria-label="后退" onClick={() => step(page, "back")}>
        <Icon name="back" size={14} />
      </button>
      <button type="button" className="ghost icon" disabled={!page.forward} title={page.forward ? "前进" : "没有下一页"} aria-label="前进" onClick={() => step(page, "forward")}>
        <Icon name="forward" size={14} />
      </button>
      {page.loading ? (
        <button type="button" className="ghost icon" title="停止加载" aria-label="停止加载" onClick={() => stop(page)}>
          <Icon name="close" size={13} />
        </button>
      ) : (
        <button type="button" className="ghost icon" title="刷新" aria-label="刷新" onClick={() => step(page, "reload")}>
          <Icon name="reload" size={14} />
        </button>
      )}
      <Icon name="globe" size={13} />
      <input
        className="web-url"
        value={draft}
        spellCheck={false}
        autoCapitalize="off"
        autoComplete="off"
        placeholder="输入网址，回车打开"
        aria-label="地址栏"
        autoFocus={page.focusBar}
        onFocus={() => {
          page.focusBar = false;
          setTyping(true);
        }}
        onBlur={() => setTyping(false)}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            setTyping(false);
            navigate(page, draft);
          } else if (e.key === "Escape") {
            setTyping(false);
            setDraft(page.url === "about:blank" ? "" : page.url);
          }
        }}
      />
      {page.loading && <span className="web-loading small muted">加载中…</span>}
    </div>
  );
}

/** Who is operating the tab now, and the way to take it over (or hand it back). */
function ControlBar({ page }: { page: Page }) {
  const controller = page.controller;
  if (!controller) return null;
  if (controller.kind === "human") {
    return (
      <div className="web-control human" role="status">
        <span>你正接管这个标签：agent 的请求会被拒绝，直到你交还。</span>
        <button type="button" className="ghost small" onClick={() => handBack(page)}>
          交还
        </button>
      </div>
    );
  }
  return (
    <div className="web-control" role="status">
      <span>
        <strong>{controller.agent}</strong> 正在操作
      </span>
      <button type="button" className="ghost small" title="接管后 agent 对这个标签的请求会收到 human_control，直到你点「交还」" onClick={() => takeOver(page)}>
        接管
      </button>
    </div>
  );
}

/** The mock preview's page: there is no `<webview>` in a plain page, so the scenario draws one. */
function MockPage({ page }: { page: Page }) {
  const shown = mockBrowser()?.page(page.tab);
  return (
    <div className="web-mock">
      <article className="web-mock-page">
        <h1>{pageName(page)}</h1>
        <p className="web-mock-url small">{page.url === "about:blank" ? "新标签页" : page.url}</p>
        {shown?.lines.length ? (
          <ul>
            {shown.lines.map((line, i) => (
              <li key={i}>{line}</li>
            ))}
          </ul>
        ) : (
          <p className="muted small">在上面的地址栏输入网址，回车打开。</p>
        )}
      </article>
      <p className="web-mock-note small muted">预览模式：这里是示例页面；在 CrossAgents 窗口里显示的是网页本身。</p>
    </div>
  );
}
