// A screenshot of the GUI's mock preview (docs/tasks/DOGFOOD.md §3), so an agent can see what
// its GUI change looks like: Vite serves `preview:mock` on a free port, a hidden Electron
// window opens a scenario and saves a PNG, then both stop. Needs no hub.
//
//   node scripts/screenshot.mjs --scenario review --out shot.png
//     [--route '#/runs/…'] [--width 1280] [--height 800] [--theme light|dark] [--wait 1500]
//     [--click '<css selector>']
//
// Prints the PNG's path as JSON on stdout: {"path": "…", "url": "…"}.

import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

function parse(argv) {
  const out = { scenario: "running", width: 1280, height: 800, wait: 1500, theme: "", route: "", click: "", out: "screenshot.png" };
  for (let i = 0; i < argv.length; i += 2) {
    const key = argv[i]?.replace(/^--/, "");
    if (!(key in out) || argv[i + 1] === undefined) throw new Error(`unknown or incomplete option ${argv[i]}`);
    out[key] = typeof out[key] === "number" ? Number(argv[i + 1]) : argv[i + 1];
  }
  if (out.theme && !["light", "dark"].includes(out.theme)) throw new Error("--theme is light or dark");
  if (out.route && !out.route.startsWith("#")) out.route = `#${out.route}`;
  return out;
}

/** This environment without ELECTRON_RUN_AS_NODE: set (say, by an Electron-based shell), the
 * binary would run as plain Node and have no windows. */
function childEnv() {
  const env = { ...process.env };
  delete env.ELECTRON_RUN_AS_NODE;
  return env;
}

async function main() {
  const opts = parse(process.argv.slice(2));
  const out = resolve(process.cwd(), opts.out);
  const require = createRequire(import.meta.url);
  // The path of the Electron binary (the package's main export, when required from Node).
  const electron = require("electron");
  const { createServer } = await import("vite");
  const server = await createServer({
    root,
    configFile: join(root, "vite.config.ts"),
    mode: "mock",
    // Its own dependency cache: re-optimizing the shared one breaks a dev server already running.
    cacheDir: join(root, "node_modules", ".vite-screenshot"),
    logLevel: "error",
    server: { host: "127.0.0.1", port: 0, strictPort: false },
  });
  await server.listen();
  try {
    const { port } = server.httpServer.address();
    const url = `http://127.0.0.1:${port}/?scenario=${encodeURIComponent(opts.scenario)}${opts.route}`;
    const code = await new Promise((done, fail) => {
      const child = spawn(electron, [join(root, "scripts", "capture.cjs")], {
        stdio: ["ignore", "inherit", "inherit"],
        env: {
          ...childEnv(),
          CA_SHOT_URL: url,
          CA_SHOT_OUT: out,
          CA_SHOT_WIDTH: String(opts.width),
          CA_SHOT_HEIGHT: String(opts.height),
          CA_SHOT_WAIT: String(opts.wait),
          CA_SHOT_THEME: opts.theme,
          CA_SHOT_CLICK: opts.click,
        },
      });
      child.on("error", fail);
      child.on("exit", done);
    });
    if (code !== 0) throw new Error(`the capture window exited with code ${code}`);
    process.stdout.write(JSON.stringify({ path: out, url }) + "\n");
  } finally {
    await server.close();
  }
}

main().catch((err) => {
  process.stderr.write(`screenshot failed: ${err instanceof Error ? err.message : err}\n`);
  process.exit(1);
});
