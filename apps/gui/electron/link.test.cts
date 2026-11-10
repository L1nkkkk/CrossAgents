// Runs compiled (`npm test` builds electron/ first). crossagents-link is fakeLink.cjs run by
// node, so these tests need neither the real program nor the network.

import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { connectArgs, decodeCode, linkError, parseEvent, parseRedeem, proxyFromResolve, hasProxy, redeemArgs, unreachableWords, type LinkProgram } from "./link.cjs";
import { checkLinkTarget, hostLabel, InputError, publicState, readRecord, tunnelReduce, writeRecord, type ConnectionState, type Crypto, type PairStep, type RemoteRecord } from "./remote.cjs";
import { RemoteController } from "./remoteController.cjs";

const ENDPOINT = "5f1c0a3e9b7d42c8a6e1f0b9d3c7e2a48b6f1d0c9e3a7b5d2f8c4e6a1b9d0f3e";

function code(fields: Record<string, unknown> = {}): string {
  const invite = { e: ENDPOINT, r: "https://relay.example./", a: ["192.168.1.20:51820"], s: "secret", h: "DESKTOP-CENTRE", x: Math.floor(Date.now() / 1000) + 1800, ...fields };
  return `ca1-${Buffer.from(JSON.stringify(invite)).toString("base64url")}`;
}

test("codes: read with line breaks, refused when they are not codes, damaged or expired", () => {
  const raw = code();
  const info = decodeCode(`  ${raw.slice(0, 30)}\n${raw.slice(30)} `);
  assert.equal(info.host, "DESKTOP-CENTRE");
  assert.deepEqual(info.link, { endpoint: ENDPOINT, relay: "https://relay.example./", addrs: ["192.168.1.20:51820"] });
  assert.throws(() => decodeCode(""), InputError);
  assert.throws(() => decodeCode("hello"), /ca1-/);
  assert.throws(() => decodeCode("ca1-%%%"), /不完整/);
  assert.throws(() => decodeCode(code({ x: 1 })), /过期/);
  assert.throws(() => decodeCode(code({ e: "not-a-key" })), /id 无效/);
  assert.throws(() => decodeCode(code({ r: "javascript:alert(1)" })), /中继/);
  // A host name that would not pass checkHost is cleaned, never passed through.
  assert.equal(decodeCode(code({ h: "--evil host;rm" })).host, "evil-host-rm");
  assert.equal(hostLabel(undefined), "center");
  // Only well-formed direct addresses survive, at most 16.
  assert.deepEqual(checkLinkTarget({ endpoint: ENDPOINT, addrs: ["1.2.3.4:5", "x; y", "[fe80::1]:7", 5] }).addrs, ["1.2.3.4:5", "[fe80::1]:7"]);
  assert.equal(decodeCode(code({ c: true, r: "https://mine.example" })).link.customRelay, true);
});

test("arguments: secrets never on the command line", () => {
  assert.deepEqual(redeemArgs("mac-gui", "macos"), ["redeem", "--name", "mac-gui", "--os", "macos"]);
  assert.throws(() => redeemArgs("-x", "macos"), InputError);
  assert.deepEqual(connectArgs({ endpoint: ENDPOINT, relay: "https://r.example", customRelay: true, addrs: ["1.2.3.4:5"] }, 50123), [
    "connect", "--endpoint", ENDPOINT, "--listen", "127.0.0.1:50123", "--relay", "https://r.example", "--custom-relay", "--addr", "1.2.3.4:5",
  ]);
  assert.deepEqual(connectArgs({ endpoint: ENDPOINT }, 1), ["connect", "--endpoint", ENDPOINT, "--listen", "127.0.0.1:1"]);
});

test("redeem output, events, proxies and error words", () => {
  assert.deepEqual(parseRedeem('{"device":{"id":"d1","name":"mac-gui"},"token":"cad_abcdefgh","host":"X"}'), { deviceId: "d1", deviceName: "mac-gui", token: "cad_abcdefgh" });
  assert.throws(() => parseRedeem("nope"), /JSON/);
  assert.throws(() => parseRedeem('{"device":{"id":"d1","name":"mac-gui"}}'), /token/);
  assert.deepEqual(parseEvent('{"event":"path","direct":true}'), { event: "path", direct: true });
  assert.equal(parseEvent("garbage"), undefined);
  assert.equal(proxyFromResolve("PROXY 127.0.0.1:7890; DIRECT"), "http://127.0.0.1:7890");
  assert.equal(proxyFromResolve("HTTPS proxy.example:443"), "https://proxy.example:443");
  assert.equal(proxyFromResolve("DIRECT"), undefined);
  assert.equal(proxyFromResolve("SOCKS5 127.0.0.1:1080"), undefined);
  assert.equal(hasProxy({ HTTPS_PROXY: " " }), false);
  assert.equal(hasProxy({ http_proxy: "http://p:1" }), true);
  assert.equal(linkError("2026 WARN noise\nError: the pairing code has expired\n"), "the pairing code has expired");
  assert.match(unreachableWords("cannot reach the hub: No addressing information available"), /中继/);
});

test("records: a pairing by code round-trips, older records read as ssh", async () => {
  const dir = mkdtempSync(join(tmpdir(), "crossagents-link-record-"));
  try {
    const file = join(dir, "remote.json");
    const record: RemoteRecord = {
      mode: "remote", via: "link", link: { endpoint: ENDPOINT, addrs: ["1.2.3.4:5"] }, target: { host: "DESKTOP-CENTRE" },
      deviceName: "mac-gui", deviceId: "d1", pairedAt: "2026-11-10T00:00:00Z", token: "c2VhbGVk",
    };
    await writeRecord(file, record);
    assert.deepEqual(await readRecord(file), record);
    assert.equal(publicState(record, undefined, "x").remote?.via, "link");
    const { via: _via, link: _link, ...old } = record;
    await writeRecord(file, old);
    assert.equal(publicState(await readRecord(file), undefined, "x").remote?.via, "ssh");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("the tunnel's path: direct or relay while connected, forgotten on reconnect", () => {
  let s = tunnelReduce(undefined, { type: "connecting" });
  s = tunnelReduce(s, { type: "path", direct: false });
  s = tunnelReduce(s, { type: "up", localPort: 5 });
  assert.deepEqual(s, { status: "connected", localPort: 5, path: "relay" });
  s = tunnelReduce(s, { type: "path", direct: true });
  assert.equal(s.path, "direct");
  s = tunnelReduce(s, { type: "down", reason: "x" });
  assert.equal(s.path, undefined);
});

// ---------------------------------------------------------------------------------------
// Pairing by code end to end, with the fake crossagents-link and a fake hub.

function fakeCrypto(): Crypto {
  return {
    unavailable: () => undefined,
    encrypt: (text) => Buffer.from(`sealed:${text}`),
    decrypt: (data) => data.toString().replace(/^sealed:/, ""),
  };
}

async function fakeHub(tokens: Set<string>): Promise<{ port: number; close(): Promise<void> }> {
  const server: Server = createServer((req, res) => {
    if (req.url === "/v1/health") return void res.writeHead(200).end('{"status":"ok","api":"v2"}');
    const auth = req.headers.authorization?.replace(/^Bearer /, "") ?? "";
    if (!tokens.has(auth)) return void res.writeHead(401).end("{}");
    res.writeHead(200).end('{"version":"0.2.15"}');
  });
  await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
  const address = server.address();
  return {
    port: typeof address === "object" && address ? address.port : 0,
    close: () => new Promise((done) => {
      server.closeAllConnections();
      server.close(() => done());
    }),
  };
}

async function until(what: string, check: () => boolean, ms = 15_000) {
  const deadline = Date.now() + ms;
  while (!check()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

test("pairing by code: redeem, forward, direct path, stop when the hub refuses the token", async () => {
  const dir = mkdtempSync(join(tmpdir(), "crossagents-link-"));
  const token = `cad_${"ab".repeat(32)}`;
  const hub = await fakeHub(new Set([token]));
  const log = join(dir, "link.log");
  const env = { ...process.env, FAKE_LINK_TOKEN: token, FAKE_LINK_HUB_PORT: String(hub.port), FAKE_LINK_LOG: log };
  const program: LinkProgram = { program: process.execPath, prefix: [join(__dirname, "fakeLink.cjs")], env: async () => env };
  const states: ConnectionState[] = [];
  const steps: PairStep[] = [];
  const changes: string[] = [];
  const c = new RemoteController({
    file: join(dir, "remote.json"),
    crypto: fakeCrypto(),
    ssh: { program: "ssh-is-not-used", prefix: [] },
    link: program,
    os: "macos",
    defaultDeviceName: "mac-gui",
    bundledHub: () => false,
    onState: (s) => states.push(s),
    onPairStep: (s) => steps.push(s),
    onHubChange: (m) => changes.push(m),
    tunnel: { upTimeoutMs: 10_000, watchMs: 100, delay: () => 50 },
  });
  try {
    await c.init();
    await assert.rejects(c.pairCode("not a code", "mac-gui"), InputError);
    assert.deepEqual(steps, [], "a bad code runs nothing");

    const state = await c.pairCode(code(), "mac-gui");
    assert.deepEqual(steps, ["redeem", "forward", "check"]);
    assert.deepEqual(changes, ["remote"]);
    assert.equal(state.remote?.via, "link");
    assert.equal(state.remote?.target.host, "DESKTOP-CENTRE");
    assert.equal(state.tunnel?.status, "connected");
    const { base, token: used } = c.hub();
    assert.equal(used, token);
    assert.equal((await fetch(`${base}/v1/info`, { headers: { authorization: `Bearer ${token}` } })).status, 200);
    await until("the direct path", () => c.get().tunnel?.path === "direct");

    // Secrets went on stdin; the token is stored only sealed.
    const calls = readFileSync(log, "utf8").trim().split("\n").map((l) => JSON.parse(l) as { args: string[] });
    assert.ok(calls.every((call) => !call.args.join(" ").includes(token) && !call.args.join(" ").includes("ca1-")));
    assert.deepEqual(calls[0].args.slice(0, 5), ["redeem", "--name", "mac-gui", "--os", "macos"]);
    const saved = JSON.parse(readFileSync(join(dir, "remote.json"), "utf8"));
    assert.equal(saved.via, "link");
    assert.ok(!JSON.stringify(saved).includes(token));

    // The hub stops accepting the token: the forward exits 3, the tunnel stops for good.
    env.FAKE_LINK_TOKEN = "cad_other_token";
    await c.reconnect();
    await until("the refused token", () => c.get().tunnel?.stopped === true);
    assert.match(c.get().tunnel?.reason ?? "", /吊销/);
  } finally {
    await c.dispose();
    await hub.close();
    rmSync(dir, { recursive: true, force: true });
  }
});

test("pairing by code: a refused code says why and saves nothing", async () => {
  const dir = mkdtempSync(join(tmpdir(), "crossagents-link-fail-"));
  const env = { ...process.env, FAKE_LINK_REDEEM_FAIL: "这个配对码已经用过、过期，或中心的 hub 重启过" };
  const c = new RemoteController({
    file: join(dir, "remote.json"),
    crypto: fakeCrypto(),
    ssh: { program: "unused", prefix: [] },
    link: { program: process.execPath, prefix: [join(__dirname, "fakeLink.cjs")], env: async () => env },
    defaultDeviceName: "mac-gui",
    bundledHub: () => true,
    onState: () => {},
    onPairStep: () => {},
    onHubChange: () => {},
  });
  try {
    await c.init();
    await assert.rejects(c.pairCode(code(), "mac-gui"), /已经用过/);
    assert.equal(c.mode(), "local");
    assert.equal(c.get().remote, undefined);
  } finally {
    await c.dispose();
    rmSync(dir, { recursive: true, force: true });
  }
});
