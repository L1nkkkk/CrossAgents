// preview:mock's `window.crossagents.browser` (run note browser/contract §3). A plain page has no
// `<webview>`, so every tab stands for a made-up page — a title and a few lines — and the agent's
// tools answer from that; the preview's own page surface (browser.tsx `MockPage`) draws it.
//
// A scenario can make the tools slow (`MockState.browserDelay`), so a screenshot catches the
//「正在操作」bar an agent's request puts on its tab while the tool runs.

import type { BrowserBridge, BrowserConsoleEntry } from "../bridge";
import type { WebTab } from "../panes";

type MockPage = { url: string; title: string; lines: string[] };

export type MockBrowser = BrowserBridge & {
  /** The webContents id this tab stands for: the page's calls are keyed by it, as in Electron. */
  attach(tab: WebTab, url: string): number;
  /** What the tab shows, for the preview's page surface. */
  page(tab: WebTab): MockPage | undefined;
  navigate(tab: WebTab, action: "back" | "forward" | "reload" | { url: string }): void;
  close(tab: WebTab): void;
};

/** The pages the preview's own scenarios open, so they read like pages rather than placeholders. */
const KNOWN: Record<string, MockPage> = {
  "https://example.com/pricing": {
    url: "https://example.com/pricing",
    title: "价格 · Acme",
    lines: ["三种方案：个人、团队、企业", "团队每月 ¥99 起，按人头算", "随时可以降级，差价按天折算", "开始免费试用 14 天"],
  },
  "https://developer.mozilla.org/zh-CN/docs/Web/HTML": {
    url: "https://developer.mozilla.org/zh-CN/docs/Web/HTML",
    title: "HTML：超文本标记语言 — MDN",
    lines: ["HTML（HyperText Markup Language）是构建 Web 的标准标记语言。", "它描述网页的结构：标题、段落、列表、链接和图片。", "本页：元素参考、全局属性、内容分类。"],
  },
};

/** A made-up page for `url`: its host (or file name) as the title, and a couple of lines. */
function pageFor(url: string): MockPage {
  const known = KNOWN[url];
  if (known) return { ...known, lines: [...known.lines] };
  if (!url || url === "about:blank") return { url: "about:blank", title: "", lines: [] };
  try {
    const parsed = new URL(url);
    if (parsed.protocol === "crossagents-file:") {
      const name = decodeURIComponent(parsed.pathname.split("/").pop() || "页面");
      return { url, title: name, lines: ["本地文件：页面由这个工作区里的文件渲染。", "只读：页面不能读这个文件夹之外的文件。"] };
    }
    return { url, title: parsed.host, lines: [`${parsed.host} 的示例页面（预览模式）。`, "在真实的 CrossAgents 窗口里，这里显示网页本身。"] };
  } catch {
    return { url, title: url, lines: ["预览模式的示例页面。"] };
  }
}

function snapshotText(page: MockPage): string {
  return [`页面：${page.title}`, `地址：${page.url}`, ...page.lines.map((line, i) => `[ref=${i + 1}] ${line}`)].join("\n");
}

/** A 1×1 PNG: a screenshot from the mock, so the tool answers with an image as the real one does. */
const SHOT = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

export function createMockBrowser(opts: { delay?: number } = {}): MockBrowser {
  const tabs = new Map<WebTab, MockPage>();
  const ids = new Map<number, WebTab>();
  const logs = new Map<WebTab, BrowserConsoleEntry[]>();
  let next = 7100;

  const tabOf = (wcId: number) => {
    const tab = ids.get(wcId);
    const page = tab ? tabs.get(tab) : undefined;
    if (!tab || !page) throw new Error("没有这个浏览器标签");
    return { tab, page };
  };
  const wait = () => (opts.delay ? new Promise((r) => setTimeout(r, opts.delay)) : Promise.resolve());

  return {
    attach(tab, url) {
      if (!tabs.has(tab)) tabs.set(tab, pageFor(url));
      for (const [id, known] of ids) if (known === tab) return id;
      const id = next++;
      ids.set(id, tab);
      return id;
    },
    page: (tab) => tabs.get(tab),
    navigate(tab, action) {
      if (typeof action !== "object") return;
      tabs.set(tab, pageFor(action.url));
    },
    close(tab) {
      tabs.delete(tab);
      logs.delete(tab);
      for (const [id, known] of [...ids]) if (known === tab) ids.delete(id);
    },
    onOpenTab: () => () => {},
    async snapshot(wcId) {
      await wait();
      const { page } = tabOf(wcId);
      return { text: snapshotText(page), url: page.url, title: page.title };
    },
    async click(wcId, ref) {
      await wait();
      const { page } = tabOf(wcId);
      if (!Number.isInteger(ref) || ref < 1 || ref > page.lines.length) throw new Error("这个元素已不在页面上或看不见，请重新 browser_snapshot");
      return { ok: true };
    },
    async type(wcId, ref, text) {
      await wait();
      tabOf(wcId);
      if (!Number.isInteger(ref) || ref < 1) throw new Error("ref 必须是正整数");
      if (typeof text !== "string") throw new Error("文字必须是字符串");
      return { ok: true };
    },
    async press(wcId, key) {
      await wait();
      tabOf(wcId);
      if (typeof key !== "string" || !key) throw new Error("按键要是 CDP 的名字，如 Enter");
      return { ok: true };
    },
    async scroll(wcId, dy) {
      await wait();
      tabOf(wcId);
      if (!Number.isFinite(dy)) throw new Error("滚动距离必须是数字");
      return { ok: true };
    },
    async screenshot(wcId) {
      await wait();
      tabOf(wcId);
      return { base64: SHOT, mime: "image/png" };
    },
    async console(wcId) {
      await wait();
      const { tab } = tabOf(wcId);
      const known = logs.get(tab) ?? [];
      return known.slice();
    },
    async fileUrl(absPath) {
      const name = absPath.split(/[\\/]/).filter(Boolean).pop() || "index.html";
      return `crossagents-file://mock/${encodeURIComponent(name)}`;
    },
    async bookmarks() {
      return {
        source: "C:/Users/me/AppData/Local/Google/Chrome/User Data/Default/Bookmarks",
        entries: [
          { kind: "url", id: "1", name: "GitHub", url: "https://github.com/" },
          { kind: "url", id: "2", name: "MDN HTML", url: "https://developer.mozilla.org/zh-CN/docs/Web/HTML" },
          {
            kind: "folder",
            id: "3",
            name: "内网工作台",
            children: [
              { kind: "url", id: "4", name: "Acme 价格", url: "https://example.com/pricing" },
              { kind: "url", id: "5", name: "iWiki", url: "https://iwiki.woa.com/" },
            ],
          },
        ],
      };
    },
    async openExternal() {
      return { ok: true };
    },
    async chromeStatus() {
      return {
        connected: false,
        port: 7421,
        extensionDir: "C:/Users/me/AppData/Roaming/CrossAgents/chrome-extension",
        tabs: [],
      };
    },
    async chromeAction() {
      return { ok: true };
    },
    async chromeActivate() {
      return { ok: true };
    },
    async chromeSyncCookies() {
      return { count: 12 };
    },
    async openExtensionFolder() {
      return { dir: "C:/Users/me/AppData/Roaming/CrossAgents/chrome-extension" };
    },
  };
}
