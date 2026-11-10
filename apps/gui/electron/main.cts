// The GUI's main process is transport only (docs/tasks/V2-GUI.md §2): it finds the hub,
// adds the bearer token, forwards HTTP requests and relays the SSE event stream. All
// business logic stays in the hub. The page talks to it through electron/preload.cts.

import { appendFile, mkdir, readFile, readdir, stat, lstat, rm, mkdtemp, writeFile } from "node:fs/promises";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { homedir, hostname, tmpdir } from "node:os";
import { existsSync, rmSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { app, BrowserWindow, dialog, ipcMain, Menu, nativeTheme, safeStorage, shell, session, protocol, type IpcMainEvent, type IpcMainInvokeEvent, type WebContents } from "electron";
import { allowed, REQUEST_TIMEOUT_MS, route, timeoutFor } from "./allowed.cjs";
import { manageLocalHub, type LocalHub } from "./localHub.cjs";
import { permissionAllowed } from "./permissions.cjs";
import { createTray, type AppTray } from "./tray.cjs";
import { startUpdates, type Updates } from "./updates.cjs";
import { assetPath } from "./extProtocol.cjs";
import { copyRoot, copyPrefix, copyName, staleCopies, runsWhenOpened, safeCopyRoot, zoneIdentifier, quarantineArgs } from "./openCopy.cjs";
import { decideSafeMode, loadingMarker, type SafeMode } from "./safeMode.cjs";
import { defaultDeviceName, remoteFile, sshProgram, type Crypto } from "./remote.cjs";
import { RemoteController, Unavailable } from "./remoteController.cjs";
import { applicationMenu, bundledHub, clientHeaders, clientOs, shortcut, trayIcon } from "./platform.cjs";
import { hasProxy, proxyFromResolve, type LinkProgram } from "./link.cjs";
import { tailscaleStatus } from "./tailscale.cjs";
import { attachGuest, guardWebview, registerBrowserIpc, setupBrowserSession } from "./browser.cjs";
import { FILE_SCHEME } from "./browserPolicy.cjs";

// Must run before app.ready. CORS remains enabled for module imports from file://.
protocol.registerSchemesAsPrivileged([{ scheme: "crossagents-ext", privileges: {
  standard: true, secure: true, supportFetchAPI: true, corsEnabled: true,
} }, { scheme: FILE_SCHEME, privileges: {
  standard: true, secure: true, supportFetchAPI: true, corsEnabled: true, stream: true,
} }]);
let safeMode: SafeMode = { enabled: false, reason: null };
const extensionLoading = loadingMarker(join(hubHome(), "gui-extension-loading"));

function restartSafeMode() {
  app.relaunch({ args: [...process.argv.slice(1).filter((arg) => arg !== "--safe-mode"), "--safe-mode"] });
  // A stuck renderer must not be able to veto recovery with beforeunload.
  app.exit(0);
}

const METHODS = new Set(["GET", "POST", "DELETE"]);
const RECONNECT_MS = 2_000;

/** Vite dev server, only in an unpackaged run (`npm run electron:dev`). */
const devServerUrl = app.isPackaged
  ? undefined
  : process.argv.find((a) => a.startsWith("--dev-server-url="))?.slice("--dev-server-url=".length);

const indexFile = join(__dirname, "..", "dist", "index.html");
/** Where the page may live; IPC from any other frame is refused. */
const appUrl = devServerUrl ? new URL(devServerUrl).origin + "/" : pathToFileURL(indexFile).href;

/** `$CROSSAGENTS_HOME`, else `~/.crossagents` — the same rule as the hub. */
function hubHome(): string {
  return process.env.CROSSAGENTS_HOME || join(homedir(), ".crossagents");
}

/**
 * Base URL and token of the running hub. Read on every call, so a restarted hub (new
 * port or token) is picked up without restarting the GUI.
 */
async function localConnection(): Promise<{ base: string; token: string }> {
  const home = hubHome();
  const hubFile = join(home, "hub.json");
  let text: string;
  try {
    text = await readFile(hubFile, "utf8");
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === "ENOENT") {
      throw new Error(`hub is not running (${hubFile} not found); start it with \`crossagents hub start\``);
    }
    throw new Error(`cannot read ${hubFile}: ${message(err)}`);
  }
  let port: unknown;
  try {
    port = JSON.parse(text).port;
  } catch (err) {
    throw new Error(`invalid ${hubFile}: ${message(err)}`);
  }
  if (!Number.isInteger(port)) throw new Error(`invalid ${hubFile}: no port`);
  const tokenFile = join(home, "token");
  let token: string;
  try {
    token = (await readFile(tokenFile, "utf8")).trim();
  } catch (err) {
    throw new Error(`cannot read ${tokenFile}: ${message(err)}`);
  }
  return { base: `http://127.0.0.1:${port}`, token };
}

/**
 * The hub this GUI ships and keeps running (electron/localHub.cts). Only in a packaged build:
 * a dev run (`npm run electron:dev`) talks to whatever hub is running, as before.
 */
let localHub: LocalHub | undefined;
/** The tray icon that keeps the GUI around while that hub runs (electron/tray.cts); packaged only. */
let tray: AppTray | undefined;
/** The GUI's own updates; packaged only. */
let updates: Updates | undefined;

/**
 * Local or remote mode (docs/MULTI-NODE.md §4.3, electron/remoteController.cts). Set up when the
 * app is ready; until then the GUI is in local mode, as it always was.
 */
let remote: RemoteController | undefined;

const isRemote = () => remote?.mode() === "remote";
let bundledHubPresent: boolean | undefined;
const hasBundledHub = () => bundledHubPresent ??= bundledHub(app.isPackaged, process.platform, process.resourcesPath, existsSync);
const NO_BUNDLED_HUB = "这个安装包不带 hub：在 设置 →「连接」 里连另一台电脑上的 hub";

/**
 * The hub to talk to now: this machine's (`hub.json` and `token`, read on every call) or, in
 * remote mode, the tunnel's local end with the device token. Rejects with `Unavailable` when
 * the tunnel is not up: nothing waits for it.
 */
async function currentHub(): Promise<{ base: string; token: string; mode: "local" | "remote" }> {
  if (remote && isRemote()) return { ...remote.hub(), mode: "remote" };
  // A package without a bundled hub does not connect to an independently running local hub either.
  if (!hasBundledHub()) throw new Error(NO_BUNDLED_HUB);
  await localHub?.ready();
  return { ...(await localConnection()), mode: "local" };
}

/** Electron's safeStorage, refused where it would store in the clear (Linux `basic_text`). */
const safeCrypto: Crypto = {
  unavailable() {
    if (!safeStorage.isEncryptionAvailable()) return "系统不提供加密存储";
    if (process.platform === "linux" && safeStorage.getSelectedStorageBackend() === "basic_text") {
      return "系统没有可用的钥匙串（basic_text），token 只能明文保存";
    }
    return undefined;
  },
  encrypt: (text) => safeStorage.encryptString(text),
  decrypt: (data) => safeStorage.decryptString(data),
};

/** Packaged only: starts managing the local hub (and checks it once, §13.3), or stops managing it. */
function manageForMode(mode: "local" | "remote") {
  if (!app.isPackaged) return;
  if (mode === "remote") {
    // Nothing local is started, upgraded or rechecked in remote mode.
    localHub = undefined;
    return;
  }
  if (localHub) return;
  if (!hasBundledHub()) return;
  const exe = process.platform === "win32" ? "crossagents.exe" : "crossagents";
  localHub = manageLocalHub(join(process.resourcesPath, "bin", exe), hubHome(), logLine);
}

/** The hub changed: event and terminal relays end, and every window reloads into the new mode. */
function hubChanged(mode: "local" | "remote") {
  logLine(`connection: now ${mode}`);
  manageForMode(mode);
  for (const controller of relays.values()) controller.abort();
  for (const mine of terminalRelays.values()) for (const controller of mine.values()) controller.abort();
  // After the IPC reply that caused it has reached the page.
  setTimeout(() => {
    for (const win of BrowserWindow.getAllWindows()) if (!win.isDestroyed()) win.webContents.reload();
  }, 300);
}

function broadcast(channel: string, value: unknown) {
  for (const win of BrowserWindow.getAllWindows()) if (!win.isDestroyed()) win.webContents.send(channel, value);
}

/**
 * crossagents-link (hole punching): packaged, next to the hub in `resources/bin`; in development
 * `CROSSAGENTS_LINK`, else the cargo build of this checkout. Started with the system proxy
 * (Chromium's answer for the relay) unless the environment names one.
 */
function linkProgram(): LinkProgram | undefined {
  const exe = process.platform === "win32" ? "crossagents-link.exe" : "crossagents-link";
  const candidates = app.isPackaged
    ? [join(process.resourcesPath, "bin", exe)]
    : [
        process.env.CROSSAGENTS_LINK,
        process.env.CARGO_TARGET_DIR && join(process.env.CARGO_TARGET_DIR, "debug", exe),
        join(app.getAppPath(), "..", "..", "target", "debug", exe),
      ].filter((p): p is string => !!p);
  const program = candidates.find((p) => existsSync(p));
  if (!program) {
    logLine(`link: no crossagents-link (looked at ${candidates.join(", ")})`);
    return undefined;
  }
  return {
    program,
    prefix: [],
    async env() {
      const env = { ...process.env, NO_PROXY: [process.env.NO_PROXY, "127.0.0.1,localhost"].filter(Boolean).join(",") };
      if (hasProxy(env)) return env;
      const proxy = proxyFromResolve(await session.defaultSession.resolveProxy("https://relay.iroh.network/").catch(() => undefined));
      return proxy ? { ...env, HTTPS_PROXY: proxy, HTTP_PROXY: proxy } : env;
    },
  };
}

function createRemote(): RemoteController {
  return new RemoteController({
    file: remoteFile(app.getPath("userData")),
    crypto: safeCrypto,
    ssh: { program: sshProgram(process.platform, process.env, existsSync), prefix: [] },
    link: linkProgram(),
    os: clientOs(process.platform),
    defaultDeviceName: defaultDeviceName(hostname()),
    bundledHub: hasBundledHub,
    onState: (state) => broadcast("connection:state", state),
    onPairStep: (step) => broadcast("connection:pair-step", step),
    onHubChange: hubChanged,
    beforeRemote: async () => {
      const previous = localHub;
      localHub = undefined;
      await previous?.ready();
    },
    log: logLine,
  });
}

/** `connection:*` answers `{ ok, state }` or `{ ok: false, error }` (electron/preload.cts). */
function connectionHandler(channel: string, call: (r: RemoteController, ...args: unknown[]) => Promise<unknown> | unknown) {
  ipcMain.handle(channel, async (event, ...args: unknown[]) => {
    try {
      if (!fromApp(event)) throw new Error("request from an unknown page");
      if (!remote) throw new Error("the GUI is still starting");
      return { ok: true, state: await call(remote, ...args) };
    } catch (err) {
      return { ok: false, error: message(err) };
    }
  });
}
connectionHandler("connection:get", (r) => r.get());
connectionHandler("connection:pair", (r, target, deviceName) => r.pair(target, deviceName));
connectionHandler("connection:pair-code", (r, code, deviceName) => r.pairCode(code, deviceName));
connectionHandler("connection:use", (r, mode) => r.use(mode));
connectionHandler("connection:reconnect", (r) => r.reconnect());
connectionHandler("connection:forget", (r) => r.forget());

const REMOTE_UNAVAILABLE = "远程模式下不可用";

function message(err: unknown): string {
  if (err instanceof Error) {
    // fetch wraps the socket error ("ECONNREFUSED ...") in `cause`.
    const cause = (err as { cause?: unknown }).cause;
    return cause instanceof Error ? `${err.message}: ${cause.message}` : err.message;
  }
  return String(err);
}

/**
 * What a failed request to the hub means for the message it carried (docs/tasks/DELIVERY-AUDIT.md
 * §3, §4). A timeout or a connection that dropped mid-flight may already have delivered it, so
 * that is `outcome_unknown` — never "unreachable", which invites a duplicate send. Only a
 * connection that was refused (or never resolved) means it certainly did not get there.
 */
function transportCode(err: unknown): "outcome_unknown" | "unreachable" {
  const e = err as { name?: unknown; code?: unknown; cause?: { name?: unknown; code?: unknown } };
  const name = `${String(e?.name ?? "")} ${String(e?.cause?.name ?? "")}`;
  if (/AbortError|TimeoutError/.test(name)) return "outcome_unknown";
  const code = String(e?.code ?? e?.cause?.code ?? "");
  if (/ECONNREFUSED|ENOTFOUND|EHOSTUNREACH|ENETUNREACH|EAI_AGAIN|ENOENT/.test(code)) return "unreachable";
  // A reset or a silently dropped connection: nobody can say whether the hub saw it.
  return "outcome_unknown";
}

/** A request that got no hub answer: `status` 0 with the error body `src/api.ts` reads. */
function transportFailure(err: unknown) {
  const code = transportCode(err);
  const what = code === "outcome_unknown" ? "the request may or may not have reached the hub" : "cannot reach the hub";
  return { ok: true as const, response: { status: 0, body: JSON.stringify({ error: { code, message: `${what}: ${message(err)}` } }) } };
}

function fromApp(event: IpcMainEvent | IpcMainInvokeEvent): boolean {
  const url = event.senderFrame?.url;
  return !!url && url.startsWith(appUrl);
}

ipcMain.handle("extensions:safe-mode", (event) => {
  if (!fromApp(event)) throw new Error("request from an unknown page");
  return safeMode;
});
ipcMain.handle("extensions:loading", (event) => {
  if (!fromApp(event)) throw new Error("request from an unknown page");
  if (!safeMode.enabled) extensionLoading.start();
});
ipcMain.handle("extensions:ready", (event) => {
  if (!fromApp(event)) throw new Error("request from an unknown page");
  extensionLoading.ready();
});

registerBrowserIpc(fromApp);

ipcMain.handle("tailscale:status", (event) => {
  if (!fromApp(event)) throw new Error("request from an unknown page");
  return tailscaleStatus();
});

async function serveExtension(request: Request): Promise<Response> {
  const cors = { "Access-Control-Allow-Origin": "*", "Access-Control-Allow-Methods": "GET, HEAD, OPTIONS" };
  if (request.method === "OPTIONS") return new Response(null, { status: 204, headers: cors });
  if (request.method !== "GET" && request.method !== "HEAD") return new Response(null, { status: 405, headers: cors });
  if (safeMode.enabled) return new Response("Extensions are disabled in safe mode", { status: 403, headers: cors });
  try {
    const path = assetPath(request.url);
    const { base, token, mode } = await currentHub();
    const asset = await fetch(base + path, {
      headers: { authorization: `Bearer ${token}`, ...clientHeaders(mode, process.platform) }, signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    });
    if (mode === "remote" && asset.status === 401) remote?.unauthorized();
    const headers = new Headers(asset.headers);
    for (const [name, value] of Object.entries(cors)) headers.set(name, value);
    return new Response(request.method === "HEAD" ? null : asset.body, { status: asset.status, headers });
  } catch (err) {
    logLine(`extension protocol: ${message(err)}`);
    return new Response("Extension asset unavailable", { status: 404, headers: cors });
  }
}

ipcMain.handle("hub:request", async (event, method: unknown, path: unknown, body: unknown) => {
  try {
    if (!fromApp(event)) throw new Error("request from an unknown page");
    if (typeof method !== "string" || !METHODS.has(method)) throw new Error(`unsupported method: ${method}`);
    if (typeof path !== "string" || !path.startsWith("/v1/")) throw new Error(`not an API path: ${path}`);
    if (!allowed(method, path)) {
      throw new Error(
        `the GUI does not call ${method} ${route(path)}; add it to ALLOWED in electron/allowed.cts if it now does`,
      );
    }
    if (body !== null && typeof body !== "string") throw new Error("body must be a string");
    let hub: Awaited<ReturnType<typeof currentHub>>;
    try {
      hub = await currentHub();
    } catch (err) {
      // The tunnel is down: say so now rather than queue the request.
      if (!(err instanceof Unavailable)) throw err;
      return { ok: true, response: { status: 503, body: JSON.stringify({ error: { code: "unavailable", message: err.message } }) } };
    }
    const { base, token } = hub;
    const headers: Record<string, string> = { authorization: `Bearer ${token}`, ...clientHeaders(hub.mode, process.platform) };
    if (body !== null) headers["content-type"] = "application/json";
    let response: Response;
    try {
      response = await fetch(base + path, {
        method,
        headers,
        body,
        signal: AbortSignal.timeout(timeoutFor(method, path)),
      });
    } catch (err) {
      return transportFailure(err);
    }
    // The device was revoked: stop reconnecting and ask for a new pairing.
    if (hub.mode === "remote" && response.status === 401) remote?.unauthorized();
    return { ok: true, response: { status: response.status, body: await response.text() } };
  } catch (err) {
    return { ok: false, error: message(err) };
  }
});

/** The development hub this GUI is talking to instead of its own, or null. */
ipcMain.handle("hub:dev", async (event) => {
  if (!fromApp(event) || isRemote()) return null;
  await localHub?.ready();
  return localHub?.devHub() ?? null;
});

/** The GUI's own update (electron/updates.cts); null where it does not update itself. */
ipcMain.handle("update:get", (event) => (fromApp(event) ? (updates?.state() ?? null) : null));
ipcMain.handle("update:check", async (event) => (fromApp(event) && updates ? updates.check() : null));
ipcMain.on("update:install", (event) => {
  if (fromApp(event)) updates?.install();
});

/** Swaps the development hub for the shipped one; resolves to an error message or null. */
ipcMain.handle("hub:use-shipped", async (event) => {
  if (!fromApp(event)) return "no local hub to manage";
  if (isRemote()) return REMOTE_UNAVAILABLE;
  if (!localHub) return "no local hub to manage";
  try {
    await localHub.useShipped();
    return null;
  } catch (err) {
    return message(err);
  }
});

/**
 * A folder chosen in the system dialog, or null when cancelled. In remote mode the folder would
 * be on this machine, not the hub's: `{ error }` (electron/preload.cts throws it).
 */
ipcMain.handle("dialog:pick-folder", async (event) => {
  if (!fromApp(event)) return null;
  if (isRemote()) return { error: REMOTE_UNAVAILABLE };
  const win = BrowserWindow.fromWebContents(event.sender);
  const options = { properties: ["openDirectory" as const] };
  const result = win ? await dialog.showOpenDialog(win, options) : await dialog.showOpenDialog(options);
  return result.canceled ? null : (result.filePaths[0] ?? null);
});

const openCopies = new Set<string>();
const DOWNLOAD_TIMEOUT_MS = 10 * 60 * 1000;
const execFileAsync = promisify(execFile);

async function checkCopyRoot(root: string) {
  if (process.platform === "win32") return;
  const info = await lstat(root);
  if (!safeCopyRoot({ directory: info.isDirectory(), symlink: info.isSymbolicLink(), uid: info.uid }, process.getuid!())) {
    throw new Error("临时副本目录不安全：必须是当前用户拥有的真实目录");
  }
}

async function markDownloaded(file: string) {
  try {
    if (process.platform === "win32") await writeFile(`${file}:Zone.Identifier`, zoneIdentifier());
    else if (process.platform === "darwin") await execFileAsync("xattr", quarantineArgs(file, Date.now() / 1000));
  } catch (err) {
    logLine(`copy origin marking: ${message(err)}`);
  }
}

async function cleanOldCopies() {
  const root = copyRoot(tmpdir());
  try {
    await checkCopyRoot(root);
    const entries = await readdir(root, { withFileTypes: true });
    const candidates = await Promise.all(entries.filter((e) => e.isDirectory()).map(async (e) => ({
      name: e.name, modified: (await stat(join(root, e.name))).mtimeMs,
    })));
    await Promise.all(staleCopies(candidates, Date.now()).map((name) => rm(join(root, name), { recursive: true, force: true })));
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code !== "ENOENT") logLine(`copy cleanup: ${message(err)}`);
  }
}

/** A scope's API prefix: a session, a run, or a task of a run. */
const FILE_SCOPE = /^\/v1\/(agents\/[^/?#]+|runs\/[^/?#]+(\/tasks\/[^/?#]+)?)$/;

/**
 * Opens a workspace file with the system's default program; resolves to an error message or
 * null. The path is never trusted as given: the hub's `files/resolve` says where it is, and
 * refuses (empty path, or 403) anything outside the scope's root.
 */
ipcMain.handle("shell:open-path", async (event, scope: unknown, path: unknown) => {
  try {
    if (!fromApp(event)) throw new Error("request from an unknown page");
    if (typeof scope !== "string" || !FILE_SCOPE.test(scope)) throw new Error(`not a file scope: ${scope}`);
    if (typeof path !== "string" || !path) throw new Error("no path to open");
    const { base, token, mode } = await currentHub();
    let response: Response;
    try {
      response = await fetch(`${base}${scope}/files/resolve`, {
        method: "POST",
        headers: { authorization: `Bearer ${token}`, "content-type": "application/json", ...clientHeaders(mode, process.platform) },
        body: JSON.stringify({ paths: [path] }),
        signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
      });
    } catch (err) {
      throw new Error(`cannot reach the hub: ${message(err)}`);
    }
    if (mode === "remote" && response.status === 401) remote?.unauthorized();
    const text = await response.text();
    if (response.status >= 400) {
      let reason = text || `HTTP ${response.status}`;
      try {
        reason = JSON.parse(text).error?.message ?? reason;
      } catch {
        // Not an API error body.
      }
      throw new Error(reason);
    }
    const entry = (JSON.parse(text) as { entries?: { path?: string | null; abs_path?: string | null }[] }).entries?.[0];
    if (!entry?.path || (mode === "local" && !entry.abs_path)) throw new Error("文件不存在或不在工作区内");
    const name = copyName(entry.path);
    if (mode === "local" ? runsWhenOpened(entry.abs_path!) : (runsWhenOpened(entry.path) || runsWhenOpened(name))) throw new Error("可执行文件或脚本不会用默认程序打开，请在查看器里阅读");
    let target = entry.abs_path!;
    if (mode === "remote") {
      const signal = AbortSignal.timeout(DOWNLOAD_TIMEOUT_MS);
      let bytes: ArrayBuffer;
      try {
        const raw = await fetch(`${base}${scope}/files/raw?path=${encodeURIComponent(entry.path)}`, {
          headers: { authorization: `Bearer ${token}`, ...clientHeaders(mode, process.platform) }, signal,
        });
        if (raw.status === 401) remote?.unauthorized();
        if (raw.status === 413) throw new Error("文件超过 64 MiB，不能下载打开");
        if (!raw.ok) throw new Error(`下载文件失败：HTTP ${raw.status}`);
        bytes = await raw.arrayBuffer();
      } catch (err) {
        if (signal.aborted) throw new Error("下载超时");
        throw err;
      }
      const root = copyRoot(tmpdir());
      await mkdir(root, { recursive: true, mode: 0o700 });
      await checkCopyRoot(root);
      const dir = await mkdtemp(copyPrefix(root));
      openCopies.add(dir);
      target = join(dir, name);
      await writeFile(target, Buffer.from(bytes));
      await markDownloaded(target);
    }
    const failure = await shell.openPath(target);
    if (failure) throw new Error(failure);
    return { copy: mode === "remote" };
  } catch (err) {
    return { error: message(err) };
  }
});

/** One event relay per page; a reload starts a new one. */
const relays = new Map<number, AbortController>();

const COLOR = /^#[0-9a-f]{3,8}$/i;

const isMac = process.platform === "darwin";

/** What the page's menu (src/menubar.tsx) may ask the window to do. */
const COMMANDS = ["zoomIn", "zoomOut", "zoomReset", "reload", "devtools", "fullscreen", "quit"] as const;
type Command = (typeof COMMANDS)[number];

function runCommand(win: BrowserWindow, command: Command) {
  const contents = win.webContents;
  const zoom = contents.getZoomLevel();
  switch (command) {
    case "zoomIn":
      return contents.setZoomLevel(Math.min(zoom + 0.5, 4));
    case "zoomOut":
      return contents.setZoomLevel(Math.max(zoom - 0.5, -3));
    case "zoomReset":
      return contents.setZoomLevel(0);
    case "reload":
      return contents.reload();
    case "devtools":
      return contents.toggleDevTools();
    case "fullscreen":
      return win.setFullScreen(!win.isFullScreen());
    case "quit":
      return win.close();
  }
}

ipcMain.on("window:command", (event, command: unknown) => {
  if (!fromApp(event) || !COMMANDS.includes(command as Command)) return;
  const win = BrowserWindow.fromWebContents(event.sender);
  if (win) runCommand(win, command as Command);
});
/** Height of the page's title bar (src/styles.css `--titlebar-h`). */
const TITLEBAR_HEIGHT = 40;

/**
 * The page's theme changed (src/lib/theme.ts). The native title bar follows light or dark, and
 * the window's own background (seen while resizing or loading) takes the page colour.
 */
ipcMain.on("theme:changed", (event, theme: unknown) => {
  if (!fromApp(event) || !theme || typeof theme !== "object") return;
  const { dark, background, foreground } = theme as { dark?: unknown; background?: unknown; foreground?: unknown };
  if (typeof dark !== "boolean") return;
  nativeTheme.themeSource = dark ? "dark" : "light";
  const win = BrowserWindow.fromWebContents(event.sender);
  if (win && typeof background === "string" && COLOR.test(background)) win.setBackgroundColor(background);
  // The window buttons sit on the page's title bar: transparent, with symbols in its ink.
  if (win && !isMac && typeof foreground === "string" && COLOR.test(foreground)) {
    win.setTitleBarOverlay({ color: "#00000000", symbolColor: foreground, height: TITLEBAR_HEIGHT });
  }
});

ipcMain.on("hub:events-start", (event) => {
  if (!fromApp(event)) return;
  const contents = event.sender;
  const known = relays.has(contents.id);
  relays.get(contents.id)?.abort();
  const controller = new AbortController();
  relays.set(contents.id, controller);
  if (!known) {
    contents.once("destroyed", () => {
      relays.get(contents.id)?.abort();
      relays.delete(contents.id);
    });
  }
  void relayEvents(contents, controller.signal);
});

async function relayEvents(contents: WebContents, signal: AbortSignal) {
  const emit = (channel: string, value: string) => {
    if (!signal.aborted && !contents.isDestroyed()) contents.send(channel, value);
  };
  const last: { id?: string } = {};
  while (!signal.aborted) {
    emit("hub:status", "connecting");
    let reason: string;
    try {
      await streamOnce(emit, last, signal);
      reason = "event stream ended";
    } catch (err) {
      reason = message(err);
    }
    if (signal.aborted) return;
    if (!isRemote()) {
      // The local hub went away (or never came up): start it again; say why if it cannot be.
      localHub?.recheck();
      reason = hasBundledHub() ? (localHub?.problem() ?? reason) : NO_BUNDLED_HUB;
    }
    emit("hub:status", `offline: ${reason}`);
    await new Promise((r) => setTimeout(r, RECONNECT_MS));
  }
}

/** Reads one SSE connection, resuming after `last.id` with `Last-Event-ID`. */
async function streamOnce(emit: (channel: string, value: string) => void, last: { id?: string }, signal: AbortSignal) {
  const { base, token, mode } = await currentHub();
  const headers: Record<string, string> = { authorization: `Bearer ${token}`, accept: "text/event-stream", ...clientHeaders(mode, process.platform) };
  if (last.id !== undefined) headers["last-event-id"] = last.id;
  // First connection: live events only (views load state over REST); reconnects resume.
  const query = last.id === undefined ? "?live=true" : "";
  const response = await fetch(`${base}/v1/events${query}`, { headers, signal });
  if (mode === "remote" && response.status === 401) remote?.unauthorized();
  if (!response.ok || !response.body) throw new Error(`hub answered ${response.status}`);
  emit("hub:status", "connected");

  // `stream: true` keeps a multi-byte character split across chunks intact.
  const decoder = new TextDecoder();
  let buf = "";
  for await (const chunk of response.body) {
    buf += decoder.decode(chunk, { stream: true }).replace(/\r/g, "");
    let end;
    while ((end = buf.indexOf("\n\n")) >= 0) {
      const block = buf.slice(0, end);
      buf = buf.slice(end + 2);
      const parsed = parseSse(block);
      if (parsed.id !== undefined && /^\d+$/.test(parsed.id)) last.id = parsed.id;
      if (parsed.data !== undefined) emit("hub:event", parsed.data);
    }
  }
}

/** Terminal output being relayed, per window and terminal. */
const terminalRelays = new Map<number, Map<string, AbortController>>();
const TERMINAL_ID = /^[0-9a-f-]{36}$/;

ipcMain.on("terminal:start", (event, id: unknown, reader: unknown) => {
  if (!fromApp(event) || typeof id !== "string" || !TERMINAL_ID.test(id)) return;
  if (typeof reader !== "string" || !TERMINAL_ID.test(reader)) return;
  const contents = event.sender;
  let mine = terminalRelays.get(contents.id);
  if (!mine) {
    const relaying = new Map<string, AbortController>();
    mine = relaying;
    terminalRelays.set(contents.id, relaying);
    contents.once("destroyed", () => {
      for (const controller of relaying.values()) controller.abort();
      terminalRelays.delete(contents.id);
    });
  }
  mine.get(id)?.abort();
  const controller = new AbortController();
  mine.set(id, controller);
  void relayTerminal(contents, id, reader, controller.signal);
});

ipcMain.on("terminal:stop", (event, id: unknown) => {
  if (!fromApp(event) || typeof id !== "string") return;
  const mine = terminalRelays.get(event.sender.id);
  mine?.get(id)?.abort();
  mine?.delete(id);
});

/** Until the shell finishes or the hub no longer has it: a dropped stream only reconnects. */
async function relayTerminal(contents: WebContents, id: string, reader: string, signal: AbortSignal) {
  const emit = (data: string) => {
    if (!signal.aborted && !contents.isDestroyed()) contents.send("terminal:frame", id, data);
  };
  while (!signal.aborted) {
    try {
      if (await terminalStreamOnce(id, reader, emit, signal)) return;
    } catch {
      // Below: said the same way as a stream that simply ended.
    }
    if (signal.aborted) return;
    emit(JSON.stringify({ type: "reconnecting" }));
    if (!isRemote()) localHub?.recheck();
    await new Promise((r) => setTimeout(r, RECONNECT_MS));
  }
}

/** Whether there is nothing more to come: the shell finished, or the hub no longer has it. */
async function terminalStreamOnce(id: string, reader: string, emit: (data: string) => void, signal: AbortSignal): Promise<boolean> {
  const { base, token, mode } = await currentHub();
  const headers = { authorization: `Bearer ${token}`, accept: "text/event-stream", ...clientHeaders(mode, process.platform) };
  const url = `${base}/v1/terminals/${encodeURIComponent(id)}/output?connection=${encodeURIComponent(reader)}`;
  const response = await fetch(url, { headers, signal });
  if (mode === "remote" && response.status === 401) remote?.unauthorized();
  if (response.status === 404) {
    emit(JSON.stringify({ type: "gone" }));
    return true;
  }
  if (!response.ok || !response.body) throw new Error(`hub answered ${response.status}`);
  const decoder = new TextDecoder();
  let buf = "";
  for await (const chunk of response.body) {
    buf += decoder.decode(chunk, { stream: true }).replace(/\r/g, "");
    let end;
    while ((end = buf.indexOf("\n\n")) >= 0) {
      const { data } = parseSse(buf.slice(0, end));
      buf = buf.slice(end + 2);
      if (data === undefined) continue;
      emit(data);
      if ((JSON.parse(data) as { type?: string }).type === "exit") return true;
    }
  }
  return false;
}

/**
 * Parses one SSE block. Comments (keep-alives) and the `event:` field are ignored: the
 * event type is also inside the JSON data.
 */
function parseSse(block: string): { id?: string; data?: string } {
  const out: { id?: string; data?: string } = {};
  for (const line of block.split("\n")) {
    const i = line.indexOf(":");
    const field = i < 0 ? line : line.slice(0, i);
    const value = i < 0 ? "" : line.slice(i + 1).replace(/^ /, "");
    if (field === "id") out.id = value;
    else if (field === "data") out.data = out.data === undefined ? value : `${out.data}\n${value}`;
  }
  return out;
}

// No new windows, no navigation away from the app, no permissions (camera, notifications…).
// Web links (e.g. an account login page) open in the system browser instead.
app.on("web-contents-created", (_event, contents) => {
  // Browser tabs (<webview>) get their own rules when they attach (electron/browser.cts).
  if (contents.getType() === "webview") return;
  contents.setWindowOpenHandler(({ url }) => {
    if (url.startsWith("https://") || url.startsWith("http://")) void shell.openExternal(url);
    return { action: "deny" };
  });
  contents.on("will-navigate", (event, url) => {
    if (!url.startsWith(appUrl)) event.preventDefault();
  });
  // Only the built-in browser's tabs may attach; their settings are forced there.
  contents.on("will-attach-webview", (event, webPreferences, params) => {
    if (!guardWebview(webPreferences, params)) event.preventDefault();
  });
  contents.on("did-attach-webview", (_event, guest) => attachGuest(guest, logLine));
});

async function createWindow() {
  const win = new BrowserWindow({
    title: "CrossAgents",
    width: 1200,
    height: 800,
    minWidth: 720,
    minHeight: 480,
    show: false,
    // No native title bar: the page draws its own (src/titlebar.tsx). The window buttons stay
    // the system's: macOS traffic lights inset into it, elsewhere an overlay on its right (so
    // Windows snap layouts keep working), recoloured with the theme.
    titleBarStyle: isMac ? "hiddenInset" : "hidden",
    ...(isMac ? { trafficLightPosition: { x: 14, y: 13 } } : { titleBarOverlay: { color: "#00000000", symbolColor: "#a6a6ad", height: TITLEBAR_HEIGHT } }),
    webPreferences: {
      preload: join(__dirname, "preload.cjs"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      webSecurity: true,
      spellcheck: false,
      // The built-in browser's tabs are <webview> tags (electron/browser.cts).
      webviewTag: true,
    },
  });
  win.once("ready-to-show", () => win.show());
  let askingRecovery = false;
  win.on("unresponsive", () => {
    if (askingRecovery) return;
    askingRecovery = true;
    void dialog.showMessageBox(win, {
      type: "warning", title: "CrossAgents", message: "窗口没有响应，要以安全模式重启吗？",
      detail: "安全模式会跳过所有扩展，后台的 hub 和 agent 会继续运行。",
      buttons: ["以安全模式重启", "继续等待"], defaultId: 0, cancelId: 1, noLink: true,
    }).then(({ response }) => { if (response === 0) restartSafeMode(); })
      .finally(() => { askingRecovery = false; });
  });
  tray?.keep(win);
  // Zoom is always available; reload and dev tools are only wired here in development.
  win.webContents.on("before-input-event", (event, input) => {
    const command = shortcut(process.platform, input, !!devServerUrl);
    if (command) {
      runCommand(win, command);
      event.preventDefault();
    }
  });
  if (devServerUrl) {
    // The dev server may still be starting.
    for (;;) {
      try {
        await win.loadURL(devServerUrl);
        break;
      } catch {
        if (win.isDestroyed()) return;
        await new Promise((r) => setTimeout(r, 500));
      }
    }
  } else {
    await win.loadFile(indexFile);
  }
}

/** A packaged GUI has no console: what it does to the local hub goes to `<home>/logs/gui.log`. */
function logLine(line: string) {
  const dir = join(hubHome(), "logs");
  void mkdir(dir, { recursive: true })
    .then(() => appendFile(join(dir, "gui.log"), `${new Date().toISOString()} ${line}
`))
    .catch(() => {});
}

// One GUI per user: two would race to start and upgrade the same local hub.
const single = app.requestSingleInstanceLock();
if (!single) app.quit();
/** Brings the window back from the tray or from behind other windows. */
function showWindow() {
  const win = BrowserWindow.getAllWindows()[0];
  if (!win) {
    void createWindow();
    return;
  }
  if (win.isMinimized()) win.restore();
  win.show();
  win.focus();
}
app.on("second-instance", showWindow);

app.whenReady().then(async () => {
  if (!single) return;
  void cleanOldCopies();
  const unfinished = extensionLoading.unfinished();
  safeMode = decideSafeMode(process.argv, unfinished);
  protocol.handle("crossagents-ext", serveExtension);
  if (unfinished && safeMode.enabled) {
    void dialog.showMessageBox({ type: "warning", title: "CrossAgents 安全模式", message: safeMode.reason!, buttons: ["确定"] });
  }
  // Which mode the GUI is in decides whether the local hub is managed at all.
  const controller = createRemote();
  await controller.init().catch((err: unknown) => logLine(`connection: cannot read the saved pairing: ${message(err)}`));
  remote = controller;
  if (app.isPackaged) {
    manageForMode(controller.mode());
    // Downloads a newer release in the background and installs it when the GUI quits; the
    // next launch then upgrades the local hub (package.json `build.publish`).
    updates = startUpdates(logLine);
    tray = createTray(
      join(process.resourcesPath, trayIcon(process.platform)),
      () => localHub,
      () => controller.remoteSummary(),
      showWindow,
      logLine,
      updates,
      restartSafeMode,
    );
  }
  session.defaultSession.setPermissionRequestHandler((_contents, permission, callback) => callback(permissionAllowed(permission)));
  session.defaultSession.setPermissionCheckHandler((_contents, permission) => permissionAllowed(permission));
  setupBrowserSession(logLine);
  // macOS needs its native application menu; other platforms use the page menu.
  const template = applicationMenu(process.platform);
  Menu.setApplicationMenu(template ? Menu.buildFromTemplate(template) : null);
  void createWindow();
  app.on("activate", showWindow);
});

// The ssh of the tunnel ends with the GUI: closed properly, then killed if quitting runs ahead.
let tunnelClosed = false;
app.on("before-quit", (event) => {
  // Local mode has no tunnel: quitting goes exactly as before.
  if (tunnelClosed || !remote || remote.mode() !== "remote") return;
  event.preventDefault();
  tunnelClosed = true;
  const done = () => app.quit();
  void Promise.race([remote.dispose(), new Promise((r) => setTimeout(r, 5_000))]).finally(done);
});
app.on("will-quit", () => {
  remote?.killNow();
  for (const dir of openCopies) { try { rmSync(dir, { recursive: true, force: true }); } catch { /* best effort */ } }
});

// With a tray the window only hides; the GUI quits from the tray's menu.
app.on("window-all-closed", () => {
  if (process.platform !== "darwin" && !tray) app.quit();
});
