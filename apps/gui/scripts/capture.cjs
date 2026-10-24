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
    const image = await win.webContents.capturePage();
    writeFileSync(env.CA_SHOT_OUT, image.toPNG());
    app.exit(0);
  } catch (err) {
    process.stderr.write(`capture failed: ${err && err.message ? err.message : err}\n`);
    app.exit(1);
  }
});
