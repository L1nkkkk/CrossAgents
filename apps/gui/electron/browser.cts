// The main-process half of the built-in browser (run note browser/contract §3). Its tabs are
// <webview> tags on the `persist:browser` session. This file guards them as they attach, gives
// the session its proxy, permissions, downloads and `crossagents-file://` scheme, and answers the
// window.crossagents.browser calls of the page (preload.cts). The rules themselves are in
// browserPolicy.cts, browserFiles.cts and browserCdp.cts.

import { randomUUID } from "node:crypto";
import { mkdirSync } from "node:fs";
import { copyFile, realpath, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { BrowserWindow, dialog, ipcMain, net, session, webContents, type DownloadItem, type IpcMainInvokeEvent, type Session, type WebContents, type WebPreferences } from "electron";
import { BrowserTab } from "./browserDriver.cjs";
import { createFileRegistry, isWithin, mimeFor } from "./browserFiles.cjs";
import {
  BROWSER_PARTITION,
  FILE_SCHEME,
  browserPermissionAllowed,
  browserProxy,
  enforceGuestPreferences,
  navigationAllowed,
  webviewAllowed,
} from "./browserPolicy.cjs";

/** Channel for a window.open / target=_blank from a tab: the page opens a new web tab. */
export const OPEN_TAB_CHANNEL = "browser:open-tab";

const browserSession = (): Session => session.fromPartition(BROWSER_PARTITION);
const fileRoots = createFileRegistry(process.platform);
const tabs = new Map<number, BrowserTab>();

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** The tab for a guest, attached on first use. Forgotten when the guest is destroyed. */
function tabOf(contents: WebContents): BrowserTab {
  let tab = tabs.get(contents.id);
  if (!tab) {
    tab = new BrowserTab(contents);
    tabs.set(contents.id, tab);
    contents.once("destroyed", () => tabs.delete(contents.id));
  }
  return tab;
}

/**
 * Called when a `<webview>` is about to attach to the page's window. Returns false for anything
 * that is not a browser tab (the caller then refuses it); a browser tab's preferences are forced.
 */
export function guardWebview(prefs: WebPreferences, params: Record<string, string>): boolean {
  if (!webviewAllowed(params)) return false;
  enforceGuestPreferences(prefs);
  return true;
}

/** Called for a browser tab once it attached (`did-attach-webview`): its navigation and windows. */
export function attachGuest(guest: WebContents, log: (line: string) => void): void {
  const limit = (event: { preventDefault(): void }, url: string) => {
    if (!navigationAllowed(url)) event.preventDefault();
  };
  guest.on("will-navigate", (event, url) => limit(event, url));
  guest.on("will-redirect", (event, url) => limit(event, url));
  guest.setWindowOpenHandler(({ url }) => {
    const host = guest.hostWebContents;
    if (navigationAllowed(url) && host && !host.isDestroyed()) host.send(OPEN_TAB_CHANNEL, url);
    return { action: "deny" };
  });
  try {
    tabOf(guest);
  } catch (err) {
    log(`browser tab ${guest.id}: ${errorText(err)}`);
  }
}

/** Serves a `crossagents-file://` request: only inside a registered folder, after symlinks. */
async function serveFile(request: Request, log: (line: string) => void): Promise<Response> {
  const deny = (status: number) => new Response(null, { status });
  if (request.method !== "GET" && request.method !== "HEAD") return deny(405);
  let resolved: { root: string; path: string };
  try {
    resolved = fileRoots.resolve(request.url);
  } catch {
    return deny(403);
  }
  try {
    const realRoot = await realpath(resolved.root);
    const real = await realpath(resolved.path);
    if (!isWithin(process.platform, realRoot, real)) return deny(403);
    if (!(await stat(real)).isFile()) return deny(404);
    const body = await net.fetch(pathToFileURL(real).toString());
    return new Response(request.method === "HEAD" ? null : body.body, {
      status: 200,
      headers: { "content-type": mimeFor(real) },
    });
  } catch (err) {
    log(`browser file ${request.url}: ${errorText(err)}`);
    return deny(404);
  }
}

/**
 * Downloads: the system's save dialog comes first. The file is written to a private folder while
 * the dialog is open (the transfer is paused); cancelling cancels the download, saving copies it.
 */
function handleDownload(item: DownloadItem, contents: WebContents | null, log: (line: string) => void): void {
  const dir = join(tmpdir(), "crossagents-downloads", randomUUID());
  const name = item.getFilename() || "download";
  mkdirSync(dir, { recursive: true });
  item.setSavePath(join(dir, name));
  item.pause();
  const host = contents ? (contents.hostWebContents ?? contents) : null;
  const win = host ? BrowserWindow.fromWebContents(host) : null;
  const options = { defaultPath: name };
  const ask = win ? dialog.showSaveDialog(win, options) : dialog.showSaveDialog(options);
  void ask
    .then(async ({ canceled, filePath }) => {
      if (canceled || !filePath) {
        item.cancel();
        await rm(dir, { recursive: true, force: true }).catch(() => undefined);
        return;
      }
      item.once("done", (_event, state) => {
        void (async () => {
          try {
            if (state === "completed") await copyFile(item.getSavePath(), filePath);
            else log(`browser download ${name}: ${state}`);
          } catch (err) {
            log(`browser download ${name}: ${errorText(err)}`);
          } finally {
            await rm(dir, { recursive: true, force: true }).catch(() => undefined);
          }
        })();
      });
      item.resume();
    })
    .catch((err: unknown) => {
      item.cancel();
      log(`browser download ${name}: ${errorText(err)}`);
    });
}

/**
 * Configures `persist:browser`: proxy (from the environment, else the system's), no permissions,
 * downloads through the save dialog, and the `crossagents-file://` scheme. Call once the app is
 * ready.
 */
export function setupBrowserSession(log: (line: string) => void): void {
  const ses = browserSession();
  const proxy = browserProxy(process.env);
  const proxying = proxy
    ? ses.setProxy({ mode: "fixed_servers", proxyRules: proxy.proxyRules, proxyBypassRules: proxy.proxyBypassRules })
    : ses.setProxy({ mode: "system" });
  proxying.catch((err: unknown) => log(`browser proxy: ${errorText(err)}`));
  ses.setPermissionRequestHandler((_contents, permission, callback) => callback(browserPermissionAllowed(permission)));
  ses.setPermissionCheckHandler((_contents, permission) => browserPermissionAllowed(permission));
  ses.on("will-download", (_event, item, contents) => handleDownload(item, contents, log));
  ses.protocol.handle(FILE_SCHEME, (request) => serveFile(request, log));
}

/**
 * The browser's IPC (preload.cts). `fromApp` says whether the sender is the app's own page. Each
 * call answers `{ ok, value }` or `{ ok: false, error }`, as the connection calls do.
 */
export function registerBrowserIpc(fromApp: (event: IpcMainInvokeEvent) => boolean): void {
  const handle = (channel: string, call: (event: IpcMainInvokeEvent, ...args: unknown[]) => unknown) => {
    ipcMain.handle(channel, async (event, ...args: unknown[]) => {
      try {
        if (!fromApp(event)) throw new Error("request from an unknown page");
        return { ok: true, value: await call(event, ...args) };
      } catch (err) {
        return { ok: false, error: errorText(err) };
      }
    });
  };

  /** The tab a call names: a browser tab of the calling page, or an error. */
  const tabFor = (event: IpcMainInvokeEvent, wcId: unknown): BrowserTab => {
    if (typeof wcId !== "number" || !Number.isInteger(wcId)) throw new Error("没有这个浏览器标签");
    const contents = webContents.fromId(wcId);
    if (
      !contents ||
      contents.isDestroyed() ||
      contents.getType() !== "webview" ||
      contents.session !== browserSession() ||
      contents.hostWebContents?.id !== event.sender.id
    ) {
      throw new Error("没有这个浏览器标签");
    }
    return tabOf(contents);
  };

  const ref = (value: unknown): number => {
    if (typeof value !== "number" || !Number.isInteger(value) || value < 1) throw new Error("ref 必须是正整数");
    return value;
  };
  const text = (value: unknown): string => {
    if (typeof value !== "string") throw new Error("文字必须是字符串");
    return value;
  };

  handle("browser:snapshot", (event, wcId) => tabFor(event, wcId).snapshot());
  handle("browser:click", async (event, wcId, r) => tabFor(event, wcId).click(ref(r)));
  handle("browser:type", async (event, wcId, r, t, submit) => tabFor(event, wcId).type(ref(r), text(t), submit === true));
  handle("browser:press", async (event, wcId, key) => tabFor(event, wcId).press(text(key)));
  handle("browser:scroll", async (event, wcId, dy) => {
    if (typeof dy !== "number" || !Number.isFinite(dy)) throw new Error("滚动距离必须是数字");
    return tabFor(event, wcId).scroll(dy);
  });
  handle("browser:screenshot", (event, wcId) => tabFor(event, wcId).screenshot());
  handle("browser:console", (event, wcId) => tabFor(event, wcId).console());
  handle("browser:file-url", async (_event, absPath) => {
    if (typeof absPath !== "string" || !absPath) throw new Error("没有文件路径");
    if (!(await stat(absPath)).isFile()) throw new Error("不是一个文件");
    return fileRoots.register(absPath);
  });
}
