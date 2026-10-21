// The GUI's main process is transport only (docs/tasks/V2-GUI.md §2): it finds the hub,
// adds the bearer token, forwards HTTP requests and relays the SSE event stream. All
// business logic stays in the hub. The page talks to it through electron/preload.cts.

import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { app, BrowserWindow, dialog, ipcMain, shell, session, type IpcMainEvent, type IpcMainInvokeEvent, type WebContents } from "electron";

const METHODS = new Set(["GET", "POST", "DELETE"]);
const REQUEST_TIMEOUT_MS = 30_000;
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
    if (body !== null && typeof body !== "string") throw new Error("body must be a string");
    const { base, token } = await connection();
    const headers: Record<string, string> = { authorization: `Bearer ${token}` };
    if (body !== null) headers["content-type"] = "application/json";
    let response: Response;
    try {
      response = await fetch(base + path, {
        method,
        headers,
        body,
        signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
      });
    } catch (err) {
      throw new Error(`cannot reach the hub: ${message(err)}`);
    }
    return { ok: true, response: { status: response.status, body: await response.text() } };
  } catch (err) {
    return { ok: false, error: message(err) };
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

/** One event relay per page; a reload starts a new one. */
const relays = new Map<number, AbortController>();

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

app.whenReady().then(() => {
  session.defaultSession.setPermissionRequestHandler((_contents, _permission, callback) => callback(false));
  session.defaultSession.setPermissionCheckHandler(() => false);
  void createWindow();
  app.on("activate", () => {
    if (BrowserWindow.getAllWindows().length === 0) void createWindow();
  });
});

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") app.quit();
});
