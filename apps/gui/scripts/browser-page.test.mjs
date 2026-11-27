// Real-DOM verification of Tauri / Chrome Bridge in-page scripts (`browser_page.js` &
// `browser_console.js`) using local headless Chrome/Edge over CDP.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import http from "node:http";
import { test } from "node:test";
import { launchCdpBrowser } from "./cdp-browser.mjs";

const pageScript = readFileSync(new URL("../src-tauri/src/browser_page.js", import.meta.url), "utf8");
const consoleScript = readFileSync(new URL("../src-tauri/src/browser_console.js", import.meta.url), "utf8");

const IFRAME_HTML = `<!doctype html>
<html><body>
  <p id="frame-note">Inside same-origin iframe</p>
  <button id="frame-btn" onclick="this.textContent='Frame Clicked'">Iframe Action</button>
</body></html>`;

const MAIN_HTML = `<!doctype html>
<html>
<head>
  <meta charset="utf-8" />
  <title>Browser Use Test Fixture</title>
  <style>
    html, body { margin: 0; padding: 0; height: 100vh; overflow: hidden; font-family: sans-serif; }
    #scroll-box { height: 220px; overflow-y: auto; border: 1px solid #ccc; padding: 8px; }
    .spacer { height: 700px; }
  </style>
</head>
<body>
  <div id="scroll-box">
    <h1>Browser Use Fixture</h1>
    <label for="email">Work Email</label>
    <input id="email" type="email" placeholder="you@example.com" />

    <label for="secret">API Secret</label>
    <input id="secret" type="password" value="super-secret-token" />

    <input id="search" type="search" aria-label="Quick Search" />
    <div id="search-result"></div>

    <label for="plan">Subscription Plan</label>
    <select id="plan">
      <option value="free">Free Tier</option>
      <option value="pro">Pro Plan</option>
      <option value="ent">Enterprise</option>
    </select>
    <div id="plan-result"></div>

    <div id="editor" contenteditable="true" role="textbox" aria-label="Notes Editor">Old note</div>

    <button id="disabled-btn" disabled>Locked Action</button>
    <button id="pointer-trigger" type="button">Open Menu</button>
    <div id="pointer-status">closed</div>

    <div id="shadow-host"></div>
    <iframe id="child-frame" src="/iframe.html" width="260" height="80"></iframe>

    <section id="section-b">
      <h2>Scoped Section</h2>
      <a id="scoped-link" href="/docs">Read Docs</a>
    </section>

    <div id="async-banner">Loading...</div>
    <div class="spacer"></div>
    <div id="bottom-marker">Bottom Reached</div>
  </div>
  <script>
    const search = document.getElementById("search");
    search.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && e.code === "Enter" && e.keyCode === 13) {
        document.getElementById("search-result").textContent = "Submitted: " + search.value;
      }
    });
    const plan = document.getElementById("plan");
    plan.addEventListener("change", () => {
      document.getElementById("plan-result").textContent = "Plan=" + plan.value;
    });
    const trigger = document.getElementById("pointer-trigger");
    trigger.addEventListener("pointerdown", (e) => {
      if (e.button === 0 && e.isPrimary) {
        document.getElementById("pointer-status").textContent = "opened-via-pointer";
      }
    });
    const host = document.getElementById("shadow-host");
    const root = host.attachShadow({ mode: "open" });
    root.innerHTML = '<button id="shadow-btn">Shadow Button</button><span id="shadow-out">idle</span>';
    root.getElementById("shadow-btn").addEventListener("click", () => {
      root.getElementById("shadow-out").textContent = "shadow-clicked";
    });
    window.triggerAsyncDone = () => {
      setTimeout(() => {
        document.getElementById("async-banner").textContent = "All Ready";
        const badge = document.createElement("div");
        badge.id = "ready-badge";
        badge.textContent = "Ready Badge";
        document.getElementById("scroll-box").appendChild(badge);
      }, 150);
    };
  </script>
</body>
</html>`;

test("browser_page.js & browser_console.js work end-to-end in real Chromium", async () => {
  const server = http.createServer((req, res) => {
    if (req.url === "/iframe.html") {
      res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      res.end(IFRAME_HTML);
      return;
    }
    res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
    res.end(MAIN_HTML);
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  const baseUrl = `http://127.0.0.1:${port}`;
  const page = await launchCdpBrowser({ width: 1024, height: 768 });

  const drive = (action) => page.evaluate(`(${pageScript})(${JSON.stringify(action)})`);
  const refFor = (snapshotText, needle) => {
    const line = snapshotText.split("\n").find((l) => l.includes(needle));
    assert.ok(line, `expected snapshot line matching ${needle} in:\n${snapshotText}`);
    const match = /\[ref=(\d+)\]/.exec(line);
    assert.ok(match, `missing [ref=N] in: ${line}`);
    return Number(match[1]);
  };

  try {
    await page.send("Page.addScriptToEvaluateOnNewDocument", { source: consoleScript });
    await page.navigate(`${baseUrl}/`);
    // Ensure iframe has also finished loading.
    await page.evaluate(`new Promise((r) => { const f = document.getElementById('child-frame'); if (f.contentDocument?.readyState === 'complete') r(); else f.addEventListener('load', () => r(), { once: true }); })`);

    // 1. Rich snapshot: labels, password masking, select options, disabled state, shadow DOM, iframe, scroll info
    const snap = await drive({ action: "snapshot" });
    assert.equal(snap.error, undefined, `snapshot failed: ${snap.error}`);
    assert.equal(snap.title, "Browser Use Test Fixture");
    assert.match(snap.text, /Viewport: 1024x768 \| Scroll: 0\/\d+px/);
    assert.match(snap.text, /input\[email\] "Work Email"/);
    assert.match(snap.text, /input\[password\] "API Secret" value="••••"/);
    assert.ok(!snap.text.includes("super-secret-token"), "password plaintext must never leak into snapshot");
    assert.match(snap.text, /select "Subscription Plan" selected="Free Tier" options=\["Free Tier"\(free\), "Pro Plan"\(pro\), "Enterprise"\(ent\)\]/);
    assert.match(snap.text, /button "Locked Action" disabled/);
    assert.match(snap.text, /button "Shadow Button"/);
    assert.match(snap.text, /button "Iframe Action"/);
    assert.match(snap.text, /href="\/docs"/);
    assert.ok(snap.text.includes("Inside same-origin iframe"), "same-origin iframe text included");

    // 2. Typing with clear=true and clear=false (append)
    const emailRef = refFor(snap.text, '"Work Email"');
    assert.deepEqual(await drive({ action: "type", reference: emailRef, text: "alice", submit: false, clear: true }), { ok: true });
    assert.deepEqual(await drive({ action: "type", reference: emailRef, text: "@acme.dev", submit: false, clear: false }), { ok: true });
    assert.equal(await page.evaluate("document.getElementById('email').value"), "alice@acme.dev");

    // 3. Submit without <form> via Enter keydown
    const searchRef = refFor(snap.text, '"Quick Search"');
    assert.deepEqual(await drive({ action: "type", reference: searchRef, text: "tauri browser", submit: true, clear: true }), { ok: true });
    assert.equal(await page.evaluate("document.getElementById('search-result').textContent"), "Submitted: tauri browser");

    // 4. browser_select by label and by value; typing into <select> hints browser_select
    const planRef = refFor(snap.text, '"Subscription Plan"');
    const badType = await drive({ action: "type", reference: planRef, text: "pro", submit: false, clear: true });
    assert.match(badType.error, /browser_select/);
    const sel1 = await drive({ action: "select", reference: planRef, value: "Pro Plan" });
    assert.deepEqual(sel1, { ok: true, selected_value: "pro", selected_label: "Pro Plan" });
    assert.equal(await page.evaluate("document.getElementById('plan-result').textContent"), "Plan=pro");
    const sel2 = await drive({ action: "select", reference: planRef, value: "ent" });
    assert.deepEqual(sel2, { ok: true, selected_value: "ent", selected_label: "Enterprise" });

    // 5. contenteditable replace and append
    const editorRef = refFor(snap.text, '"Notes Editor"');
    assert.deepEqual(await drive({ action: "type", reference: editorRef, text: "Updated note", submit: false, clear: true }), { ok: true });
    assert.equal(await page.evaluate("document.getElementById('editor').textContent"), "Updated note");
    assert.deepEqual(await drive({ action: "type", reference: editorRef, text: " + extra", submit: false, clear: false }), { ok: true });
    assert.equal(await page.evaluate("document.getElementById('editor').textContent"), "Updated note + extra");

    // 6. Disabled click refused; pointerdown-only trigger works; Shadow DOM & iframe clicks work
    const disabledRef = refFor(snap.text, '"Locked Action"');
    const disabledClick = await drive({ action: "click", reference: disabledRef });
    assert.match(disabledClick.error, /disabled/);

    const pointerRef = refFor(snap.text, '"Open Menu"');
    assert.deepEqual(await drive({ action: "click", reference: pointerRef }), { ok: true });
    assert.equal(await page.evaluate("document.getElementById('pointer-status').textContent"), "opened-via-pointer");

    const shadowRef = refFor(snap.text, '"Shadow Button"');
    assert.deepEqual(await drive({ action: "click", reference: shadowRef }), { ok: true });
    assert.equal(await page.evaluate("document.getElementById('shadow-host').shadowRoot.getElementById('shadow-out').textContent"), "shadow-clicked");

    const iframeRef = refFor(snap.text, '"Iframe Action"');
    assert.deepEqual(await drive({ action: "click", reference: iframeRef }), { ok: true });
    assert.equal(await page.evaluate("document.getElementById('child-frame').contentDocument.getElementById('frame-btn').textContent"), "Frame Clicked");

    // 7. Key chords & Tab navigation
    await page.evaluate("document.getElementById('email').focus()");
    assert.deepEqual(await drive({ action: "press", key: "Control+a" }), { ok: true });
    assert.equal(await page.evaluate("window.getSelection().toString() || document.getElementById('email').value.slice(document.getElementById('email').selectionStart, document.getElementById('email').selectionEnd)"), "alice@acme.dev");
    assert.deepEqual(await drive({ action: "press", key: "Tab" }), { ok: true });
    assert.equal(await page.evaluate("document.activeElement.id"), "secret");
    assert.deepEqual(await drive({ action: "press", key: "Shift+Tab" }), { ok: true });
    assert.equal(await page.evaluate("document.activeElement.id"), "email");

    // 8. Smart inner-container scroll (body has overflow: hidden)
    const scrolled = await drive({ action: "scroll", dy: 400 });
    assert.equal(scrolled.ok, true);
    assert.ok(scrolled.scroll_y > 0, `expected inner scroll container to scroll, got: ${JSON.stringify(scrolled)}`);
    assert.ok(scrolled.max_scroll_y >= scrolled.scroll_y);

    // 9. Scoped snapshot
    const scoped = await drive({ action: "snapshot", selector: "#section-b", max_chars: 500 });
    assert.match(scoped.text, /Read Docs/);
    assert.ok(!scoped.text.includes("Work Email"), "scoped snapshot should exclude elements outside #section-b");

    // 10. Probe for browser_wait (including invalid selector fast-fail & async match)
    const badProbe = await drive({ action: "probe", selector: "div > > :not(" });
    assert.match(badProbe.error, /无效的 CSS 选择器/);
    const beforeAsync = await drive({ action: "probe", text: "All Ready", text_gone: "Loading...", selector: "#ready-badge" });
    assert.equal(beforeAsync.matched, false);
    await page.evaluate("window.triggerAsyncDone()");
    let matched = false;
    for (let i = 0; i < 20; i++) {
      await new Promise((r) => setTimeout(r, 40));
      const p = await drive({ action: "probe", text: "All Ready", text_gone: "Loading...", selector: "#ready-badge", url: "127.0.0.1" });
      if (p.matched) {
        matched = true;
        break;
      }
    }
    assert.equal(matched, true, "probe should match once async DOM settles");

    // 11. Console capture: console.warn + uncaught error + unhandledrejection
    await page.evaluate(`(() => {
      console.warn("careful-warning", { n: 7 });
      setTimeout(() => { throw new Error("boom-uncaught"); }, 0);
      setTimeout(() => { Promise.reject(new Error("boom-rejection")); }, 0);
    })()`);
    await new Promise((r) => setTimeout(r, 80));
    const logs = await drive({ action: "console" });
    assert.ok(logs.some((e) => e.level === "warn" && e.text.includes("careful-warning")), JSON.stringify(logs));
    assert.ok(logs.some((e) => e.level === "error" && e.text.includes("boom-uncaught")), JSON.stringify(logs));
    assert.ok(logs.some((e) => e.level === "error" && e.text.includes("boom-rejection")), JSON.stringify(logs));
  } finally {
    await page.close();
    server.close();
  }
});
