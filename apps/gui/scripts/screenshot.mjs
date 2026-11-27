// A screenshot of the GUI's mock preview (docs/tasks/DOGFOOD.md §3), so an agent can see what
// its GUI change looks like: Vite serves `preview:mock` on a free port, headless Chrome / Edge
// opens a scenario over CDP and saves a PNG, then both stop. Needs no hub.
//
//   node scripts/screenshot.mjs --scenario review --out shot.png
//     [--route '#/runs/…'] [--width 1280] [--height 800] [--theme light|dark] [--wait 1500]
//     [--click '<css selector>']
//
// Prints the PNG's path as JSON on stdout: {"path": "…", "url": "…"}.

import { writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { launchCdpBrowser } from "./cdp-browser.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

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

async function main() {
  const opts = parse(process.argv.slice(2));
  const out = resolve(process.cwd(), opts.out);
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
  let page = null;
  const deadline = setTimeout(() => {
    process.stderr.write("capture timed out\n");
    process.exit(2);
  }, 90_000);
  deadline.unref();
  try {
    const { port } = server.httpServer.address();
    const url = `http://127.0.0.1:${port}/?scenario=${encodeURIComponent(opts.scenario)}${opts.route}`;
    page = await launchCdpBrowser({
      width: opts.width,
      height: opts.height,
      noSandbox: !!process.env.CA_SHOT_NO_SANDBOX,
    });
    await page.navigate(url);
    if (opts.theme) {
      // The app keeps its theme in localStorage (src/lib/theme.ts); set it and load again.
      const choice = JSON.stringify({ mode: opts.theme, paper: process.env.CA_SHOT_PAPER || "plain" });
      await page.evaluate(`localStorage.setItem("crossagents.theme", ${JSON.stringify(choice)})`);
      await page.navigate(url);
    }
    // The scenario panel is the preview's own, not the app's.
    await page.insertCSS("#mock-panel { display: none !important; }");
    // Ensure focus-gated polling (like quota) runs even in headless mode.
    await page.evaluate(`document.hasFocus = () => true; window.dispatchEvent(new Event("focus"));`);
    await sleep(opts.wait);
    if (opts.click) {
      // A state only a click reaches (a confirmation's answer from the hub): click, settle.
      const clicked = await page.evaluate(
        `(() => { const el = document.querySelector(${JSON.stringify(opts.click)}); if (!el) return false; el.click(); return true; })()`,
      );
      if (!clicked) throw new Error(`nothing to click: ${opts.click}`);
      await sleep(600);
    }
    if (process.env.CA_SHOT_FOCUS) {
      await page.evaluate(
        `(() => { const el = document.querySelector(${JSON.stringify(process.env.CA_SHOT_FOCUS)}); if (el) el.focus(); })()`,
      );
      await sleep(300);
    }
    writeFileSync(out, await page.screenshot());
    process.stdout.write(JSON.stringify({ path: out, url }) + "\n");
  } finally {
    clearTimeout(deadline);
    await page?.close();
    await server.close();
  }
}

main().catch((err) => {
  process.stderr.write(`${err?.message ?? err}\n`);
  process.exit(1);
});
