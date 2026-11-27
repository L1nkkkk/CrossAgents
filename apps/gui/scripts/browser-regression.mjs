// Runs browser integration & native-wheel UI regressions using local headless Chrome / Edge
// over CDP + Vite; no user hub, credentials or model calls, and never downloads software.
import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { launchCdpBrowser } from "./cdp-browser.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const ui = process.argv.includes("--ui");
const baseline = process.argv.find((a) => a.startsWith("--baseline="))?.split("=")[1];
const outArg = process.argv.find((a) => a.startsWith("--out="))?.slice(6);
const baselineFiles = [
  "src/hub.ts",
  "src/views/Home.tsx",
  "src/modelPick.tsx",
  "src/views/Agents.tsx",
  "src/views/Run.tsx",
  "src/views/SessionComposer.tsx",
  "src/views/Settings.tsx",
  "src/styles.css",
];
const sources = new Map(
  baseline
    ? baselineFiles.map((file) => [
        join(root, file).replaceAll("\\", "/"),
        execFileSync("git", ["show", `${baseline}:apps/gui/${file}`], { cwd: root, encoding: "utf8" }),
      ])
    : [],
);

async function runUiRegression(page, url) {
  const js = (code) => page.evaluate(code);
  const until = async (code) => {
    const start = Date.now();
    while (!(await js(code))) {
      if (Date.now() - start > 12_000) throw new Error(`timeout: ${code}`);
      await sleep(20);
    }
    return Date.now() - start;
  };
  const wheel = async (selector, deltaY = 160, deltaX = 0) => {
    // End Chromium's wheel transaction before changing target or testing an edge.
    await sleep(650);
    const point = await js(
      `(() => { const r=document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect(); return {x:Math.round(Math.max(5,Math.min(innerWidth-5,r.left+r.width/2))),y:Math.round(Math.max(45,Math.min(innerHeight-5,r.top+Math.min(r.height/2,60))))}; })()`,
    );
    await page.mouseMove(point.x, point.y);
    await page.mouseWheel(point.x, point.y, deltaX, deltaY);
    await sleep(250);
  };
  const positions = (selector) =>
    js(
      `(() => {const e=document.querySelector(${JSON.stringify(selector)});return {inner:e.scrollTop,x:e.scrollLeft,outer:document.querySelector('.convo')?.scrollTop??0,left:document.querySelector('.sidebar .scroll')?.scrollTop??0,window:scrollY};})()`,
    );

  await page.navigate(`${url}/?scenario=model-search`);
  await until("!!document.querySelector('.mode-pick')");
  await js("[...document.querySelectorAll('.mode-pick button')].find(b=>b.textContent.includes('Chat')).click()");
  const cold = await until("!!document.querySelector('.chip-model button')");
  await sleep(1000);
  await js("[...document.querySelectorAll('.mode-pick button')].find(b=>b.textContent.includes('Work')).click()");
  await sleep(50);
  await js("[...document.querySelectorAll('.mode-pick button')].find(b=>b.textContent.includes('Chat')).click()");
  const warm = await until("!!document.querySelector('.chip-model button')");
  const readBefore = await js("window.__catalogReads");
  await js("location.hash='#/usage'");
  await until("!document.querySelector('.mode-pick')");
  await js("location.hash='#/'");
  await until("!!document.querySelector('.mode-pick')");
  const returning = await until("!!document.querySelector('.chip-model button')");
  await js("document.querySelector('.chip-model button').click()");
  await until("!!document.querySelector('.menu-options .menu-item')");
  const menu = await js(
    "({options:document.querySelectorAll('.menu-options .menu-item').length,reads:window.__catalogReads,overflow:getComputedStyle(document.querySelector('.menu-options')).overflowY})",
  );
  await wheel(".menu-options");
  menu.afterWheel = await positions(".menu-options");
  await js("document.querySelector('.menu-options').scrollTop=1e6");
  await wheel(".menu-options");
  menu.atBoundary = await positions(".menu-options");
  console.log(JSON.stringify({ ui: true, coldChatMs: cold, warmWorkToChatMs: warm, returnChatMs: returning, readBefore, menu }, null, 2));

  await page.navigate(`${url}/?scenario=diagram`);
  await sleep(2000);
  await until("!!document.querySelector('.convo-inner')");
  await js(`(() => {
    document.getElementById('mock-panel')?.remove();
    const host=document.querySelector('.convo-inner');
    const pre=document.createElement('pre');pre.id='wheel-code';pre.className='cli-out';pre.textContent=Array.from({length:100},(_,i)=>'line '+i+' '+ 'code '.repeat(100)).join('\\n');host.prepend(pre);
    const notice=document.createElement('div');notice.className='ask ask-notice';notice.innerHTML='<div id="wheel-panel" class="ask-text opened"></div>';notice.firstChild.textContent='expanded notice\\n'.repeat(150);host.prepend(notice);
    const space=document.createElement('div');space.style.height='1200px';space.style.flexShrink='0';host.append(space);
    const side=document.querySelector('.sidebar .scroll');const list=document.createElement('div');list.style.height='2000px';side.append(list);
    const input=document.querySelector('textarea'); input.id='wheel-input';input.value='input line\\n'.repeat(200);input.style.height='90px';input.style.overflow='auto';
  })()`);
  const matrix = {};
  for (const selector of ["#wheel-code", "#wheel-panel"]) {
    await js(`document.querySelector(${JSON.stringify(selector)}).scrollIntoView({block:'start'});document.querySelector(${JSON.stringify(selector)}).scrollTop=0`);
    await sleep(200);
    const before = await positions(selector);
    await wheel(selector);
    const inside = await positions(selector);
    await js(`document.querySelector(${JSON.stringify(selector)}).scrollTop=1e6`);
    await sleep(100);
    const boundaryBefore = await positions(selector);
    await wheel(selector);
    const boundaryAfter = await positions(selector);
    await js(
      `document.querySelector(${JSON.stringify(selector)}).scrollIntoView({block:'start'});document.querySelector('.convo').scrollTop+=40;document.querySelector(${JSON.stringify(selector)}).scrollTop=0`,
    );
    await sleep(100);
    const topBefore = await positions(selector);
    await wheel(selector, -160);
    const topAfter = await positions(selector);
    matrix[selector] = { before, inside, boundaryBefore, boundaryAfter, topBefore, topAfter };
  }
  await js(`(() => {
    const input = document.querySelector('#wheel-input');
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
    setter.call(input, 'input line\\n'.repeat(200));
    input.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  await sleep(50);
  await js("const input=document.querySelector('#wheel-input');input.style.height='90px';input.style.overflow='auto';input.scrollTop=0;");
  const inputBefore = await positions("#wheel-input");
  await wheel("#wheel-input");
  const inputAfter = await positions("#wheel-input");
  await js("document.querySelector('#wheel-input').scrollTop=1e6");
  await wheel("#wheel-input");
  matrix.input = { inputBefore, inputAfter, boundary: await positions("#wheel-input") };

  await js("document.querySelector('.sidebar .scroll').scrollTop=0");
  const sideBefore = await positions(".sidebar .scroll");
  await wheel(".sidebar .scroll");
  const sideAfter = await positions(".sidebar .scroll");
  await js("document.querySelector('.sidebar .scroll').scrollTop=1e6");
  await wheel(".sidebar .scroll");
  matrix.sidebar = { sideBefore, sideAfter, boundary: await positions(".sidebar .scroll") };

  await js("import('/src/panes.ts').then(m=>m.openTool('activity'))");
  await until("!!document.querySelector('.tool-body')");
  await js("const p=document.createElement('div');p.style.height='2000px';p.textContent='controlled right pane';document.querySelector('.tool-body').append(p)");
  const rightBefore = await positions(".tool-body");
  await wheel(".tool-body");
  const rightAfter = await positions(".tool-body");
  await js("document.querySelector('.tool-body').scrollTop=1e6");
  await wheel(".tool-body");
  matrix.right = { rightBefore, rightAfter, boundary: await positions(".tool-body") };

  await js(
    `(() => {const e=document.createElement('pre');e.id='wheel-horizontal';e.className='diffstat';e.textContent='wide code '.repeat(500);document.querySelector('.convo-inner').prepend(e);e.scrollIntoView({block:'center'});})()`,
  );
  const horizontalBefore = await positions("#wheel-horizontal");
  await wheel("#wheel-horizontal", 0, 160);
  const horizontalAfter = await positions("#wheel-horizontal");
  await js("document.querySelector('#wheel-horizontal').scrollLeft=1e6");
  await wheel("#wheel-horizontal", 0, 160);
  matrix.horizontal = { horizontalBefore, horizontalAfter, boundary: await positions("#wheel-horizontal") };

  await js(
    `(() => {const backdrop=document.createElement('div');backdrop.className='dialog-backdrop';backdrop.id='wheel-backdrop';const e=document.createElement('div');e.className='dialog';e.id='wheel-dialog';const content=document.createElement('div');content.style.cssText='height:1600px;flex-shrink:0';content.textContent='controlled modal';e.append(content);backdrop.append(e);document.body.append(backdrop);})()`,
  );
  const modalBefore = await positions("#wheel-dialog");
  await wheel("#wheel-dialog");
  const modalAfter = await positions("#wheel-dialog");
  await js("document.querySelector('#wheel-dialog').scrollTop=1e6");
  await wheel("#wheel-dialog");
  matrix.modal = { modalBefore, modalAfter, boundary: await positions("#wheel-dialog") };
  await js("document.getElementById('wheel-backdrop').remove()");
  console.log(JSON.stringify({ nativeWheelMatrix: matrix }, null, 2));

  const assert = (ok, message) => {
    if (!ok) throw new Error(message);
  };
  const isBaseline = baseline === "f69d86e";
  for (const key of ["#wheel-code", "#wheel-panel"]) {
    const row = matrix[key];
    assert(row.inside.inner > row.before.inner && row.inside.outer === row.before.outer, `${key}: inner scroll priority`);
    assert(isBaseline ? row.boundaryAfter.outer === row.boundaryBefore.outer : row.boundaryAfter.outer > row.boundaryBefore.outer, `${key}: bottom boundary ${isBaseline ? "blocked on baseline" : "chains"}`);
    assert(isBaseline ? row.topAfter.outer === row.topBefore.outer : row.topAfter.outer < row.topBefore.outer, `${key}: top boundary ${isBaseline ? "blocked on baseline" : "chains"}`);
  }
  assert(menu.afterWheel.inner > 0 && menu.atBoundary.window === 0 && menu.atBoundary.left === 0, "model menu scrolls independently");
  assert(inputAfter.inner > inputBefore.inner && inputAfter.outer === inputBefore.outer, "input inner priority");
  if (!isBaseline) assert(sideAfter.inner > sideBefore.inner && sideAfter.outer === sideBefore.outer, "left pane independence");
  assert(rightAfter.inner > rightBefore.inner && rightAfter.outer === rightBefore.outer && rightAfter.left === rightBefore.left, "right pane independence");
  assert(horizontalAfter.x > horizontalBefore.x && horizontalAfter.outer === horizontalBefore.outer, "horizontal code priority");
  assert(modalAfter.inner > modalBefore.inner && modalAfter.outer === modalBefore.outer && matrix.modal.boundary.outer === modalBefore.outer, "modal independence");
  assert(Object.values(matrix).every((row) => Object.values(row).every((p) => p.window === 0)), "no window displacement");
  if (!isBaseline) assert(returning < 100 && menu.reads === readBefore, "return mount retains models without another request");
  return {
    baseline: isBaseline ? "f69d86e" : undefined,
    catalogDelayMs: 800,
    coldChatMs: cold,
    warmWorkToChatMs: warm,
    returnChatMs: returning,
    readBefore,
    menu,
    nativeWheelMatrix: matrix,
  };
}

const server = await createServer({
  root,
  configFile: join(root, "vite.config.ts"),
  mode: "mock",
  logLevel: "error",
  cacheDir: join(root, "node_modules/.vite-regression"),
  server: { host: "127.0.0.1", port: 0, strictPort: false },
  plugins: [
    {
      name: "regression-controlled-sources",
      enforce: "pre",
      load(id) {
        return sources.get(id.split("?")[0].replaceAll("\\", "/"));
      },
      transform(code, id) {
        if (!ui) return;
        if (id.endsWith("/src/dev/MockBridge.ts")) return code.replace("for (const step of this.script)", "for (const step of this.script.slice(0, 0))");
        if (id.endsWith("/src/main.tsx")) {
          return code.replace(
            "if (start) {",
            `if (start) {
      localStorage.setItem('crossagents.mode', JSON.stringify('run'));
      const original = window.crossagents.request.bind(window.crossagents);
      window.__catalogReads = 0;
      window.crossagents.request = async (...args) => {
        if (args[1] === '/v1/plugins') { window.__catalogReads++; await new Promise(r => setTimeout(r, 800)); }
        return original(...args);
      };
    `,
          );
        }
      },
    },
    {
      name: "hub-regression-page",
      configureServer(srv) {
        srv.middlewares.use("/hub-regression", async (_req, res) => {
          res.setHeader("Content-Type", "text/html");
          res.end(await srv.transformIndexHtml("/hub-regression", '<div id="root"></div><script type="module" src="/scripts/hub-integration.tsx"></script>'));
        });
      },
    },
  ],
});
await server.listen();
const baseUrl = `http://127.0.0.1:${server.httpServer.address().port}`;
let page = null;
const deadline = setTimeout(() => {
  console.error("regression timed out");
  process.exit(2);
}, 60_000);
deadline.unref();
try {
  page = await launchCdpBrowser({
    width: 1280,
    height: 800,
    noSandbox: !!process.env.CA_SHOT_NO_SANDBOX,
    onConsoleError: (msg) => console.error(msg),
  });
  if (ui) {
    const result = await runUiRegression(page, baseUrl);
    if (outArg) {
      writeFileSync(outArg, JSON.stringify({ browser: page.browser, viewport: [1280, 800], ...result }, null, 2) + "\n");
    }
    process.exitCode = 0;
  } else {
    await page.navigate(`${baseUrl}/hub-regression`);
    for (let i = 0; i < 100 && !(await page.evaluate("!!window.regression")); i++) await sleep(50);
    const result = await page.evaluate("window.regression");
    console.log(JSON.stringify(result, null, 2));
    process.exitCode = result?.ok ? 0 : 1;
  }
} catch (error) {
  console.error(error);
  process.exitCode = 1;
} finally {
  clearTimeout(deadline);
  await page?.close();
  await server.close();
}
