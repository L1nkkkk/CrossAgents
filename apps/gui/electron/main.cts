// The GUI's main process is transport only (docs/tasks/V2-GUI.md §2): it finds the hub,
// adds the bearer token, forwards HTTP requests and relays the SSE event stream. All
// business logic stays in the hub. The page talks to it through electron/preload.cts.

import { appendFile, mkdir, readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { app, BrowserWindow, dialog, ipcMain, Menu, nativeTheme, shell, session, type IpcMainEvent, type IpcMainInvokeEvent, type WebContents } from "electron";
import { allowed, REQUEST_TIMEOUT_MS, route, timeoutFor } from "./allowed.cjs";
import { manageLocalHub, type LocalHub } from "./localHub.cjs";
import { createTray, type AppTray } from "./tray.cjs";
import { startUpdates, type Updates } from "./updates.cjs";

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
async function connection(): Promise<{ base: string; token: string }> {
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

function message(err: unknown): string {
  if (err instanceof Error) {
    // fetch wraps the socket error ("ECONNREFUSED ...") in `cause`.
    const cause = (err as { cause?: unknown }).cause;
    return cause instanceof Error ? `${err.message}: ${cause.message}` : err.message;
  }
  return String(err);
}

function fromApp(event: IpcMainEvent | IpcMainInvokeEvent): boolean {
  const url = event.senderFrame?.url;
  return !!url && url.startsWith(appUrl);
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
    await localHub?.ready();
    const { base, token } = await connection();
    const headers: Record<string, string> = { authorization: `Bearer ${token}` };
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
      throw new Error(`cannot reach the hub: ${message(err)}`);
    }
    return { ok: true, response: { status: response.status, body: await response.text() } };
  } catch (err) {
    return { ok: false, error: message(err) };
  }
});

/** The development hub this GUI is talking to instead of its own, or null. */
ipcMain.handle("hub:dev", async (event) => {
  if (!fromApp(event)) return null;
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
  if (!fromApp(event) || !localHub) return "no local hub to manage";
  try {
    await localHub.useShipped();
    return null;
  } catch (err) {
    return message(err);
  }
});

/** A folder chosen in the system dialog, or null when cancelled. */
ipcMain.handle("dialog:pick-folder", async (event) => {
  if (!fromApp(event)) return null;
  const win = BrowserWindow.fromWebContents(event.sender);
  const options = { properties: ["openDirectory" as const] };
  const result = win ? await dialog.showOpenDialog(win, options) : await dialog.showOpenDialog(options);
  return result.canceled ? null : (result.filePaths[0] ?? null);
});

/** A scope's API prefix: a session, a run, or a task of a run. */
const FILE_SCOPE = /^\/v1\/(agents\/[^/?#]+|runs\/[^/?#]+(\/tasks\/[^/?#]+)?)$/;

/**
 * Files the system would run rather than show: an agent can write one and link it, so opening
 * it with its default program is refused (the in-app viewer still shows it).
 */
const RUNS_WHEN_OPENED =
  /\.(exe|com|bat|cmd|ps1|psm1|vbs|vbe|js|jse|wsf|wsh|msi|msp|scr|pif|cpl|hta|lnk|url|reg|jar|app|command|sh|desktop|appimage)$/i;

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
    await localHub?.ready();
    const { base, token } = await connection();
    let response: Response;
    try {
      response = await fetch(`${base}${scope}/files/resolve`, {
        method: "POST",
        headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
        body: JSON.stringify({ paths: [path] }),
        signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
      });
    } catch (err) {
      throw new Error(`cannot reach the hub: ${message(err)}`);
    }
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
    if (!entry?.path || !entry.abs_path) throw new Error("文件不存在或不在工作区内");
    if (RUNS_WHEN_OPENED.test(entry.abs_path)) throw new Error("可执行文件或脚本不会用默认程序打开，请在查看器里阅读");
    const failure = await shell.openPath(entry.abs_path);
    return failure || null;
  } catch (err) {
    return message(err);
  }
});

/** One event relay per page; a reload starts a new one. */
const relays = new Map<number, AbortController>();

const COLOR = /^#[0-9a-f]{3,8}$/i;

const isMac = process.platform === "darwin";

/** What the page's menu (src/menubar.tsx) may ask the window to do. */
const COMMANDS = ["zoomIn", "zoomOut", "zoomReset", "reload", "devtools", "fullscreen", "quit"] as const;
type Command = (typeof COMMANDS)[number];
const ZOOM_KEYS: Record<string, Command> = { "=": "zoomIn", "+": "zoomIn", "-": "zoomOut", "0": "zoomReset" };

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
    await localHub?.ready();
    let reason: string;
    try {
      await streamOnce(emit, last, signal);
      reason = "event stream ended";
    } catch (err) {
      reason = message(err);
    }
    if (signal.aborted) return;
    // The local hub went away (or never came up): start it again; say why if it cannot be.
    localHub?.recheck();
    reason = localHub?.problem() ?? reason;
    emit("hub:status", `offline: ${reason}`);
    await new Promise((r) => setTimeout(r, RECONNECT_MS));
  }
}

/** Reads one SSE connection, resuming after `last.id` with `Last-Event-ID`. */
async function streamOnce(emit: (channel: string, value: string) => void, last: { id?: string }, signal: AbortSignal) {
  const { base, token } = await connection();
  const headers: Record<string, string> = { authorization: `Bearer ${token}`, accept: "text/event-stream" };
  if (last.id !== undefined) headers["last-event-id"] = last.id;
  // First connection: live events only (views load state over REST); reconnects resume.
  const query = last.id === undefined ? "?live=true" : "";
  const response = await fetch(`${base}/v1/events${query}`, { headers, signal });
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
  contents.setWindowOpenHandler(({ url }) => {
    if (url.startsWith("https://") || url.startsWith("http://")) void shell.openExternal(url);
    return { action: "deny" };
  });
  contents.on("will-navigate", (event, url) => {
    if (!url.startsWith(appUrl)) event.preventDefault();
  });
  contents.on("will-attach-webview", (event) => event.preventDefault());
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
    },
  });
  win.once("ready-to-show", () => win.show());
  tray?.keep(win);
  // No native menu, so the shortcuts it carried are wired here: zoom always (the page's own
  // menu, src/menubar.tsx, lists them); reload and dev tools only in development.
  win.webContents.on("before-input-event", (event, input) => {
    if (input.type !== "keyDown" || !input.control || input.alt) return;
    const command = ZOOM_KEYS[input.key];
    if (command) {
      runCommand(win, command);
      event.preventDefault();
    }
  });
  if (devServerUrl) {
    win.webContents.on("before-input-event", (event, input) => {
      if (input.type !== "keyDown") return;
      const key = input.key.toLowerCase();
      if (input.key === "F12" || (input.control && input.shift && key === "i")) {
        win.webContents.toggleDevTools();
        event.preventDefault();
      } else if (input.control && !input.shift && key === "r") {
        win.webContents.reload();
        event.preventDefault();
      }
    });
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

app.whenReady().then(() => {
  if (!single) return;
  if (app.isPackaged) {
    const exe = process.platform === "win32" ? "crossagents.exe" : "crossagents";
    localHub = manageLocalHub(join(process.resourcesPath, "bin", exe), hubHome(), logLine);
    // Downloads a newer release in the background and installs it when the GUI quits; the
    // next launch then upgrades the local hub (package.json `build.publish`).
    updates = startUpdates(logLine);
    tray = createTray(join(process.resourcesPath, "icon.ico"), localHub, showWindow, logLine, updates);
  }
  session.defaultSession.setPermissionRequestHandler((_contents, _permission, callback) => callback(false));
  session.defaultSession.setPermissionCheckHandler(() => false);
  // No File / Edit / View / Window bar: everything lives in the page. Copy and paste still work.
  Menu.setApplicationMenu(null);
  void createWindow();
  app.on("activate", () => {
    if (BrowserWindow.getAllWindows().length === 0) void createWindow();
  });
});

// With a tray the window only hides; the GUI quits from the tray's menu.
app.on("window-all-closed", () => {
  if (process.platform !== "darwin" && !tray) app.quit();
});
