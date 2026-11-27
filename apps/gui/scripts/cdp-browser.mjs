// Zero-dependency headless Chromium runner over Chrome DevTools Protocol (CDP), using Node's
// built-in WebSocket and fetch against a locally installed Chrome / Edge / Chromium binary.
// Shared by `scripts/screenshot.mjs` (`gui_screenshot`) and `scripts/browser-regression.mjs`.

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function onPath(names) {
  const cmd = process.platform === "win32" ? "where" : "which";
  for (const name of names) {
    const res = spawnSync(cmd, [name], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
    if (res.status === 0) {
      const hit = res.stdout.split(/\r?\n/).map((s) => s.trim()).find((s) => s && existsSync(s));
      if (hit) return hit;
    }
  }
  return null;
}

/** Finds a locally installed Chromium-based browser; never downloads software. */
export function findBrowser() {
  const override = (process.env.CA_BROWSER || process.env.CHROME_PATH || "").trim();
  if (override) {
    if (!existsSync(override)) throw new Error(`CA_BROWSER / CHROME_PATH does not exist: ${override}`);
    return override;
  }
  const candidates = [];
  if (process.platform === "win32") {
    const pf = process.env.ProgramFiles || "C:\\Program Files";
    const pf86 = process.env["ProgramFiles(x86)"] || "C:\\Program Files (x86)";
    const local = process.env.LOCALAPPDATA || "";
    candidates.push(
      join(pf, "Google", "Chrome", "Application", "chrome.exe"),
      join(pf86, "Google", "Chrome", "Application", "chrome.exe"),
      local ? join(local, "Google", "Chrome", "Application", "chrome.exe") : "",
      join(pf86, "Microsoft", "Edge", "Application", "msedge.exe"),
      join(pf, "Microsoft", "Edge", "Application", "msedge.exe"),
      local ? join(local, "Microsoft", "Edge", "Application", "msedge.exe") : "",
    );
  } else if (process.platform === "darwin") {
    candidates.push(
      "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
      "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
      "/Applications/Chromium.app/Contents/MacOS/Chromium",
    );
  }
  for (const path of candidates) {
    if (path && existsSync(path)) return path;
  }
  const found = onPath(
    process.platform === "win32"
      ? ["chrome.exe", "msedge.exe"]
      : ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "microsoft-edge"],
  );
  if (found) return found;
  throw new Error("No local Chrome, Edge or Chromium found; install one or set CA_BROWSER=/path/to/browser");
}

function waitForDebugPort(child, timeoutMs = 20_000) {
  return new Promise((resolve, reject) => {
    let stderr = "";
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error(`Browser did not expose DevTools within ${timeoutMs}ms:\n${stderr}`));
    }, timeoutMs);
    const onData = (chunk) => {
      stderr += chunk.toString("utf8");
      const match = /DevTools listening on ws:\/\/[^:\s]+:(\d+)\//i.exec(stderr);
      if (match) {
        cleanup();
        resolve(Number(match[1]));
      }
    };
    const onExit = (code, signal) => {
      cleanup();
      reject(new Error(`Browser exited before DevTools started (code=${code}, signal=${signal}):\n${stderr}`));
    };
    const onError = (err) => {
      cleanup();
      reject(err);
    };
    const cleanup = () => {
      clearTimeout(timer);
      child.stderr.off("data", onData);
      child.off("exit", onExit);
      child.off("error", onError);
    };
    child.stderr.on("data", onData);
    child.on("exit", onExit);
    child.on("error", onError);
  });
}

async function pageTargetUrl(port) {
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`http://127.0.0.1:${port}/json/list`);
      if (res.ok) {
        const list = await res.json();
        const page = list.find((item) => item.type === "page" && item.webSocketDebuggerUrl);
        if (page) return page.webSocketDebuggerUrl;
      }
    } catch {
      // DevTools HTTP endpoint may still be starting.
    }
    await sleep(50);
  }
  throw new Error(`Could not locate a DevTools page target on 127.0.0.1:${port}`);
}

/** Launches a headless Chromium browser and returns a lightweight CDP session wrapper. */
export async function launchCdpBrowser({ width = 1280, height = 800, noSandbox = false, onConsoleError } = {}) {
  const executable = findBrowser();
  const userDataDir = mkdtempSync(join(tmpdir(), "crossagents-cdp-"));
  const args = [
    "--headless=new",
    "--remote-debugging-port=0",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-background-networking",
    "--disable-background-timer-throttling",
    "--disable-renderer-backgrounding",
    "--disable-extensions",
    "--disable-gpu",
    "--no-proxy-server",
    "--mute-audio",
    `--window-size=${width},${height}`,
    `--user-data-dir=${userDataDir}`,
  ];
  if (noSandbox) args.push("--no-sandbox");
  args.push("about:blank");

  const child = spawn(executable, args, { stdio: ["ignore", "ignore", "pipe"] });
  let ws = null;
  let closed = false;

  const cleanupDir = async () => {
    for (let i = 0; i < 6; i++) {
      try {
        rmSync(userDataDir, { recursive: true, force: true });
        return;
      } catch {
        await sleep(100);
      }
    }
  };

  const close = async () => {
    if (closed) return;
    closed = true;
    try {
      ws?.close();
    } catch {
      // Ignore close errors.
    }
    if (child.exitCode === null && !child.killed) {
      child.kill();
      await new Promise((resolve) => {
        const timer = setTimeout(() => {
          try {
            child.kill("SIGKILL");
          } catch {
            // Already exited.
          }
          resolve();
        }, 3000);
        child.once("exit", () => {
          clearTimeout(timer);
          resolve();
        });
      });
    }
    await cleanupDir();
  };

  try {
    const port = await waitForDebugPort(child);
    const versionInfo = await fetch(`http://127.0.0.1:${port}/json/version`).then((r) => r.json()).catch(() => ({}));
    const wsUrl = await pageTargetUrl(port);
    ws = new WebSocket(wsUrl);
    await new Promise((resolve, reject) => {
      ws.addEventListener("open", resolve, { once: true });
      ws.addEventListener("error", (ev) => reject(new Error(`CDP WebSocket error: ${ev.message || "connection failed"}`)), { once: true });
    });

    let seq = 1;
    const pending = new Map();
    const listeners = new Map();

    ws.addEventListener("message", (event) => {
      const msg = JSON.parse(typeof event.data === "string" ? event.data : Buffer.from(event.data).toString("utf8"));
      if (msg.id) {
        const waiter = pending.get(msg.id);
        if (!waiter) return;
        pending.delete(msg.id);
        if (msg.error) waiter.reject(new Error(`CDP ${waiter.method}: ${msg.error.message}`));
        else waiter.resolve(msg.result);
        return;
      }
      if (msg.method === "Runtime.consoleAPICalled" && msg.params?.type === "error" && onConsoleError) {
        const text = (msg.params.args || []).map((a) => a.value ?? a.description ?? "").join(" ");
        onConsoleError(text);
      }
      const set = listeners.get(msg.method);
      if (set) for (const fn of [...set]) fn(msg.params);
    });

    ws.addEventListener("close", () => {
      for (const [, waiter] of pending) waiter.reject(new Error("CDP connection closed"));
      pending.clear();
    });

    const send = (method, params = {}) =>
      new Promise((resolve, reject) => {
        const id = seq++;
        pending.set(id, { resolve, reject, method });
        ws.send(JSON.stringify({ id, method, params }));
      });

    const onceEvent = (method, timeoutMs = 30_000) =>
      new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          listeners.get(method)?.delete(handler);
          reject(new Error(`Timed out waiting for CDP event ${method}`));
        }, timeoutMs);
        const handler = (params) => {
          clearTimeout(timer);
          listeners.get(method)?.delete(handler);
          resolve(params);
        };
        if (!listeners.has(method)) listeners.set(method, new Set());
        listeners.get(method).add(handler);
      });

    await send("Page.enable");
    await send("Runtime.enable");
    await send("Emulation.setDeviceMetricsOverride", {
      width,
      height,
      deviceScaleFactor: 1,
      mobile: false,
    });

    const navigate = async (url) => {
      const loaded = onceEvent("Page.loadEventFired");
      const nav = await send("Page.navigate", { url });
      if (nav.errorText) throw new Error(`Navigation failed (${url}): ${nav.errorText}`);
      await loaded;
    };

    const evaluate = async (expression) => {
      const res = await send("Runtime.evaluate", {
        expression,
        awaitPromise: true,
        returnByValue: true,
      });
      if (res.exceptionDetails) {
        const detail = res.exceptionDetails.exception?.description || res.exceptionDetails.text || "evaluation error";
        throw new Error(detail);
      }
      return res.result?.value;
    };

    const insertCSS = async (css) => {
      await evaluate(`(() => {
        const style = document.createElement("style");
        style.textContent = ${JSON.stringify(css)};
        (document.head || document.documentElement).appendChild(style);
      })()`);
    };

    const screenshot = async () => {
      const { data } = await send("Page.captureScreenshot", { format: "png" });
      return Buffer.from(data, "base64");
    };

    const mouseMove = (x, y) => send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y });
    const mouseWheel = (x, y, deltaX, deltaY) =>
      send("Input.dispatchMouseEvent", { type: "mouseWheel", x, y, deltaX, deltaY });

    return {
      browser: versionInfo.Browser || executable,
      send,
      navigate,
      evaluate,
      insertCSS,
      screenshot,
      mouseMove,
      mouseWheel,
      close,
    };
  } catch (err) {
    await close();
    throw err;
  }
}
