import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { Readable } from "node:stream";
import type { IncomingMessage } from "node:http";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";

/// `$CROSSAGENTS_HOME`, else `~/.crossagents` — the same rule as the hub.
function hubHome(): string {
  return process.env.CROSSAGENTS_HOME || join(homedir(), ".crossagents");
}

async function readBody(req: IncomingMessage): Promise<string | undefined> {
  const chunks: Buffer[] = [];
  for await (const chunk of req) chunks.push(chunk as Buffer);
  return chunks.length ? Buffer.concat(chunks).toString("utf8") : undefined;
}

/// Dev-server only: lets the frontend run in a plain browser (`npm run dev`) by forwarding
/// `/v1/*` to the hub with the bearer token, like the Electron main process does in the app.
function hubProxy(): Plugin {
  return {
    name: "crossagents-hub-proxy",
    configureServer(server) {
      server.middlewares.use("/v1", async (req, res) => {
        const abort = new AbortController();
        res.on("close", () => abort.abort());
        try {
          const home = hubHome();
          const { port } = JSON.parse(readFileSync(join(home, "hub.json"), "utf8"));
          const token = readFileSync(join(home, "token"), "utf8").trim();
          const headers: Record<string, string> = { authorization: `Bearer ${token}` };
          for (const name of ["content-type", "accept", "last-event-id"]) {
            const value = req.headers[name];
            if (typeof value === "string") headers[name] = value;
          }
          const upstream = await fetch(`http://127.0.0.1:${port}${req.originalUrl}`, {
            method: req.method,
            headers,
            body: req.method === "GET" || req.method === "HEAD" ? undefined : await readBody(req),
            signal: abort.signal,
          });
          res.writeHead(upstream.status, {
            "content-type": upstream.headers.get("content-type") ?? "application/octet-stream",
            "cache-control": "no-cache",
          });
          // Send headers now: a live event stream may have nothing to write for a while.
          res.flushHeaders();
          if (upstream.body) {
            const body = Readable.fromWeb(upstream.body as never);
            // Aborting a stream (the page reloads mid-SSE) errors it; unhandled, that kills Vite.
            body.on("error", () => res.destroy());
            body.pipe(res);
          } else {
            res.end();
          }
        } catch (err) {
          if (abort.signal.aborted) return;
          res.writeHead(502, { "content-type": "application/json" });
          res.end(JSON.stringify({ error: { code: "internal", message: `cannot reach the hub: ${err}` } }));
        }
      });
    },
  };
}

/// Dev-server only: the React refresh preamble is an inline script and HMR uses a WebSocket,
/// which the strict CSP in index.html forbids. Builds keep the strict policy.
function devCsp(): Plugin {
  return {
    name: "crossagents-dev-csp",
    apply: "serve",
    transformIndexHtml: (html) =>
      html
        .replace("script-src 'self'", "script-src 'self' 'unsafe-inline'")
        .replace("connect-src 'self'", "connect-src 'self' ws://localhost:* ws://127.0.0.1:*"),
  };
}

/** The commit this page is built from (`__BUILD__.commit`); undefined outside a git checkout. */
function gitCommit(): string | undefined {
  if (process.env.CROSSAGENTS_COMMIT) return process.env.CROSSAGENTS_COMMIT;
  try {
    return execFileSync("git", ["rev-parse", "--short=7", "HEAD"], { encoding: "utf8" }).trim() || undefined;
  } catch {
    return undefined;
  }
}

export default defineConfig(({ command, mode }) => {
  // `npm run preview:mock` (or VITE_MOCK=1): the page answers itself with made-up data, no hub
  // needed (src/dev/). Everything behind `__MOCK__` is dropped from other builds.
  const mock = mode === "mock" || process.env.VITE_MOCK === "1";
  return {
    // Relative asset URLs: the packaged app loads dist/index.html from a file:// URL.
    base: "./",
    plugins: [react(), ...(mock ? [] : [hubProxy()]), devCsp()],
    define: {
      __MOCK__: JSON.stringify(mock),
      __BUILD__: JSON.stringify({
        version: JSON.parse(readFileSync(new URL("package.json", import.meta.url), "utf8")).version,
        commit: gitCommit(),
        dev: command === "serve",
      }),
    },
    resolve: { alias: { "@bindings": fileURLToPath(new URL("../../bindings", import.meta.url)) } },
    clearScreen: false,
    server: { port: 1420, strictPort: !mock, watch: { ignored: ["**/electron/**", "**/dist-electron/**", "**/release/**"] } },
    build: { target: "es2022" },
  };
});
