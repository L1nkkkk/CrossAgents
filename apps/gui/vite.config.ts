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
/// `/v1/*` to the hub with the bearer token, like the Tauri side does in the app.
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
          if (upstream.body) Readable.fromWeb(upstream.body as never).pipe(res);
          else res.end();
        } catch (err) {
          if (abort.signal.aborted) return;
          res.writeHead(502, { "content-type": "application/json" });
          res.end(JSON.stringify({ error: { code: "internal", message: `cannot reach the hub: ${err}` } }));
        }
      });
    },
  };
}

export default defineConfig({
  plugins: [react(), hubProxy()],
  resolve: { alias: { "@bindings": fileURLToPath(new URL("../../bindings", import.meta.url)) } },
  clearScreen: false,
  server: { port: 1420, strictPort: true, watch: { ignored: ["**/src-tauri/**"] } },
  build: { target: "es2022" },
});
