import assert from "node:assert/strict";
import { test } from "node:test";
import type { WebPreferences } from "electron";
import { browserPermissionAllowed, browserProxy, enforceGuestPreferences, navigationAllowed, webviewAllowed } from "./browserPolicy.cjs";

test("a browser <webview> is the browser partition, attached by the app's page", () => {
  // Electron does not forward data-* attributes, so the marker is not required.
  assert.equal(webviewAllowed({ partition: "persist:browser" }, true), true);
  assert.equal(webviewAllowed({ partition: "persist:browser", src: "https://example.com/" }, true), true);
  // Partition wrong → refused.
  assert.equal(webviewAllowed({ partition: "persist:other" }, true), false);
  assert.equal(webviewAllowed({}, true), false);
  // Host not the app → refused.
  assert.equal(webviewAllowed({ partition: "persist:browser" }, false), false);
  // Start page outside the allowed schemes → refused; empty and about:blank allowed.
  assert.equal(webviewAllowed({ partition: "persist:browser", src: "file:///C:/secret.txt" }, true), false);
  assert.equal(webviewAllowed({ partition: "persist:browser", src: "javascript:alert(1)" }, true), false);
  assert.equal(webviewAllowed({ partition: "persist:browser", src: "about:blank" }, true), true);
  assert.equal(webviewAllowed({ partition: "persist:browser", src: "" }, true), true);
  assert.equal(webviewAllowed({ partition: "persist:browser", src: "crossagents-file://abc/index.html" }, true), true);
});

test("an allowed tab gets its preferences forced; a refused one is not touched", () => {
  const asked = { preload: "/x.cjs", nodeIntegration: true, contextIsolation: false, sandbox: false } as WebPreferences;
  // The same two steps guardWebview runs in browser.cts.
  if (webviewAllowed({ partition: "persist:browser" }, true)) enforceGuestPreferences(asked);
  assert.equal(asked.nodeIntegration, false);
  assert.equal(asked.contextIsolation, true);
  assert.equal(asked.sandbox, true);
  assert.equal("preload" in asked, false);
  const refused = { preload: "/x.cjs", nodeIntegration: true } as WebPreferences;
  if (webviewAllowed({ partition: "persist:other" }, true)) enforceGuestPreferences(refused);
  assert.equal(refused.nodeIntegration, true);
});

test("a browser tab's preferences are forced whatever the page asked for", () => {
  const prefs = {
    preload: "/app/preload.cjs",
    preloadURL: "file:///app/preload.cjs",
    nodeIntegration: true,
    nodeIntegrationInSubFrames: true,
    contextIsolation: false,
    sandbox: false,
    webSecurity: false,
    webviewTag: true,
    allowRunningInsecureContent: true,
  } as WebPreferences;
  enforceGuestPreferences(prefs);
  assert.equal("preload" in prefs, false);
  assert.equal("preloadURL" in prefs, false);
  assert.equal(prefs.nodeIntegration, false);
  assert.equal(prefs.nodeIntegrationInSubFrames, false);
  assert.equal(prefs.contextIsolation, true);
  assert.equal(prefs.sandbox, true);
  assert.equal(prefs.webSecurity, true);
  assert.equal(prefs.webviewTag, false);
  assert.equal(prefs.allowRunningInsecureContent, false);
});

test("tabs may navigate to web pages and local files, nothing else", () => {
  assert.equal(navigationAllowed("https://example.com/a"), true);
  assert.equal(navigationAllowed("http://127.0.0.1:8080/"), true);
  assert.equal(navigationAllowed("crossagents-file://abc/index.html"), true);
  assert.equal(navigationAllowed("file:///C:/secret.txt"), false);
  assert.equal(navigationAllowed("crossagents-ext://x/y/z"), false);
  assert.equal(navigationAllowed("javascript:alert(1)"), false);
  assert.equal(navigationAllowed("about:blank"), false);
  assert.equal(navigationAllowed("not a url"), false);
});

test("browser tabs get no permission", () => {
  for (const p of ["camera", "microphone", "geolocation", "notifications", "clipboard-read", "clipboard-sanitized-write", "media", "fullscreen"]) {
    assert.equal(browserPermissionAllowed(p), false, p);
  }
});

test("the proxy comes from the environment in the order the link program uses, else the system", () => {
  assert.equal(browserProxy({}), undefined);
  assert.equal(browserProxy({ HTTPS_PROXY: "   " }), undefined);
  assert.deepEqual(browserProxy({ HTTP_PROXY: "http://127.0.0.1:7890" }), { proxyRules: "http://127.0.0.1:7890" });
  assert.deepEqual(browserProxy({ HTTP_PROXY: "127.0.0.1:7890/" }), { proxyRules: "http://127.0.0.1:7890" });
  assert.deepEqual(browserProxy({ HTTP_PROXY: "http://a:1", HTTPS_PROXY: "http://b:2" }), { proxyRules: "http://b:2" });
  assert.deepEqual(browserProxy({ all_proxy: "socks5://127.0.0.1:1080", http_proxy: "http://a:1" }), { proxyRules: "socks5://127.0.0.1:1080" });
  assert.deepEqual(browserProxy({ HTTPS_PROXY: "http://a:1", NO_PROXY: " localhost , .corp.example ,," }), {
    proxyRules: "http://a:1",
    proxyBypassRules: "localhost,.corp.example",
  });
});
