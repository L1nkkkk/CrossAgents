// Electron side of scripts/screenshot.mjs: an offscreen window opens CA_SHOT_URL, waits for
// the page to settle and writes a PNG to CA_SHOT_OUT. Never shown.

const { app, BrowserWindow } = require("electron");
const { writeFileSync } = require("node:fs");

const env = process.env;
const width = Number(env.CA_SHOT_WIDTH) || 1280;
const height = Number(env.CA_SHOT_HEIGHT) || 800;
const wait = Number(env.CA_SHOT_WAIT) || 1500;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// A stuck page must not hold the caller forever.
setTimeout(() => {
  process.stderr.write("capture timed out\n");
  app.exit(2);
}, 90_000).unref();

app.disableHardwareAcceleration();
// The page is served on loopback: a system proxy must not sit in front of it.
app.commandLine.appendSwitch("no-proxy-server");
// Where the OS's own sandbox keeps the renderer or GPU process from starting (a locked-down
// machine), CA_SHOT_NO_SANDBOX=1 runs them in-process and unsandboxed. The window only ever
// loads the preview served on loopback.
if (env.CA_SHOT_NO_SANDBOX) {
  app.commandLine.appendSwitch("no-sandbox");
  app.commandLine.appendSwitch("in-process-gpu");
}
app.whenReady().then(async () => {
  try {
    const win = new BrowserWindow({ show: false, width, height, useContentSize: true, webPreferences: { offscreen: true } });
    await win.loadURL(env.CA_SHOT_URL);
    if (env.CA_SHOT_THEME) {
      // The app keeps its theme in localStorage (src/lib/theme.ts); set it and load again.
      const choice = JSON.stringify({ mode: env.CA_SHOT_THEME, paper: "plain" });
      await win.webContents.executeJavaScript(`localStorage.setItem("crossagents.theme", ${JSON.stringify(choice)})`);
      await win.loadURL(env.CA_SHOT_URL);
    }
    // The scenario panel is the preview's own, not the app's.
    await win.webContents.insertCSS("#mock-panel { display: none !important; }");
    await sleep(wait);
    if (env.CA_SHOT_CLICK) {
      // A state only a click reaches (a confirmation's answer from the hub): click, settle.
      const clicked = await win.webContents.executeJavaScript(
        `(() => { const el = document.querySelector(${JSON.stringify(env.CA_SHOT_CLICK)}); if (!el) return false; el.click(); return true; })()`,
      );
      if (!clicked) throw new Error(`nothing to click: ${env.CA_SHOT_CLICK}`);
      await sleep(600);
    }
    const image = await win.webContents.capturePage();
    writeFileSync(env.CA_SHOT_OUT, image.toPNG());
    app.exit(0);
  } catch (err) {
    process.stderr.write(`capture failed: ${err && err.message ? err.message : err}\n`);
    app.exit(1);
  }
});
