// Runs compiled (`npm test` builds electron/ first). The ssh is fakeSsh.cjs run by node, so
// these tests need no ssh on the machine.

import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import {
  backoff, checkCommand, checkDeviceName, checkHost, checkPort, checkTarget, checkUser, defaultDeviceName, execArgs,
  InputError, NOT_RUNNING, openToken, pairCommand, parseDeviceAdd, parseHubFile, publicState, readRecord, remoteCommand,
  REPAIR, REVOKED, sealToken, sshProgram, tunnelArgs, tunnelReduce, writeRecord, type ConnectionState, type Crypto,
  type PairStep, type RemoteRecord, type TunnelState,
} from "./remote.cjs";
import { RemoteController, Unavailable } from "./remoteController.cjs";
import { locate, pairDevice, type Ssh } from "./remoteTunnel.cjs";

// ---------------------------------------------------------------------------------------
// Checks

test("hosts: names, ~/.ssh/config aliases and bracketed IPv6 only", () => {
  for (const ok of ["center", "my-box.local", "10.0.0.2", "[::1]", "[fe80::1%eth0]", "host_alias"]) assert.equal(checkHost(ok), ok);
  for (const bad of ["", "-oProxyCommand=calc", "a b", "a;b", "user@host", "::1", "host:22", "$(x)", "a\nb", "[::1]x", 5]) {
    assert.throws(() => checkHost(bad), InputError, String(bad));
  }
});

test("users, ports, device names, commands", () => {
  assert.equal(checkUser(""), undefined);
  assert.equal(checkUser("link.z"), "link.z");
  for (const bad of ["-l", "a b", "a@b", "a;b"]) assert.throws(() => checkUser(bad), InputError);
  assert.equal(checkPort(22), 22);
  assert.equal(checkPort(undefined), undefined);
  for (const bad of [0, 65536, 2.5, "22", -1]) assert.throws(() => checkPort(bad), InputError);
  assert.equal(checkDeviceName("laptop-gui"), "laptop-gui");
  assert.equal(checkDeviceName("a".repeat(64)), "a".repeat(64));
  for (const bad of ["", "-x", ".x", "a".repeat(65), "a b", "a/b", "名字"]) assert.throws(() => checkDeviceName(bad), InputError);
  assert.equal(checkCommand("  "), undefined);
  assert.equal(checkCommand("C:\\Program Files\\CrossAgents\\crossagents.exe"), "C:\\Program Files\\CrossAgents\\crossagents.exe");
  assert.equal(checkCommand("/opt/crossagents/bin/crossagents"), "/opt/crossagents/bin/crossagents");
  for (const bad of ['a"b', "$HOME/x", "`x`", "%PATH%", "a!b", "a&b", "a|b", "a;b", "a<b", "a>b", "a^b", "a\nb", "a\rb"]) {
    assert.throws(() => checkCommand(bad), InputError, bad);
  }
  assert.deepEqual(checkTarget({ host: "center", user: "", sshPort: 2222, command: " " }), { host: "center", sshPort: 2222 });
});

test("the default device name is the host name, cleaned, with -gui", () => {
  assert.equal(defaultDeviceName("LINK-PC"), "LINK-PC-gui");
  assert.equal(defaultDeviceName("my mac (2)"), "my-mac-2-gui");
  assert.equal(defaultDeviceName("--x"), "x-gui");
  assert.equal(defaultDeviceName("电脑"), "crossagents-gui");
  assert.equal(defaultDeviceName("a".repeat(100)).length, 64);
  checkDeviceName(defaultDeviceName("a".repeat(100)));
});

// ---------------------------------------------------------------------------------------
// ssh's arguments

test("Windows uses its own OpenSSH first, else ssh on PATH", () => {
  const env = { SystemRoot: "C:\\Windows" };
  const system = join("C:\\Windows", "System32", "OpenSSH", "ssh.exe");
  assert.equal(sshProgram("win32", env, (p) => p === system), system);
  assert.equal(sshProgram("win32", env, () => false), "ssh.exe");
  assert.equal(sshProgram("darwin", env, () => true), "ssh");
});

test("ssh arguments: never prompt, the host after --, the forward on loopback both ends", () => {
  const target = { host: "[::1]", user: "u", sshPort: 2222 };
  assert.deepEqual(execArgs(target, "cat ~/.crossagents/hub.json"), [
    "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-l", "u", "-p", "2222", "--", "::1", "cat ~/.crossagents/hub.json",
  ]);
  assert.deepEqual(tunnelArgs({ host: "center" }, 50001, 7420), [
    "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-N", "-o", "ExitOnForwardFailure=yes", "-o", "ServerAliveInterval=15",
    "-o", "ServerAliveCountMax=3", "-L", "127.0.0.1:50001:127.0.0.1:7420", "--", "center",
  ]);
  // StrictHostKeyChecking is the person's own setting.
  assert.ok(!tunnelArgs({ host: "center" }, 1, 2).some((a) => a.includes("StrictHostKeyChecking")));
});

test("the command: the person's > hub.json's exe > crossagents, paths double-quoted", () => {
  const hub = { port: 7420, exe: "/home/u/.crossagents/bin/0.2.10/crossagents" };
  assert.equal(remoteCommand({ host: "h", command: "/opt/ca" }, hub), '"/opt/ca"');
  assert.equal(remoteCommand({ host: "h" }, hub), '"/home/u/.crossagents/bin/0.2.10/crossagents"');
  assert.equal(remoteCommand({ host: "h" }, { port: 1, exe: 'C:\\x"&calc' }), "crossagents");
  assert.equal(remoteCommand({ host: "h" }, { port: 1 }), "crossagents");
  assert.equal(pairCommand('"/opt/ca"', "laptop-gui"), '"/opt/ca" device add laptop-gui --json');
  assert.throws(() => pairCommand("crossagents", "x; rm -rf ~"), InputError);
});

test("hub.json and device add output are read strictly", () => {
  assert.deepEqual(parseHubFile('\uFEFF{"pid":1,"port":7420,"version":"0.2.10","exe":"C:\\\\x.exe"}\r\n'), { port: 7420, exe: "C:\\x.exe" });
  assert.deepEqual(parseHubFile('{"pid":1,"port":7420,"version":"0.1.0"}'), { port: 7420 });
  assert.throws(() => parseHubFile('{"port":0}'));
  assert.throws(() => parseHubFile("not json"));
  const token = `cad_${"ab".repeat(32)}`;
  assert.deepEqual(parseDeviceAdd(JSON.stringify({ device: { id: "d1", name: "n" }, token })), { deviceId: "d1", deviceName: "n", token });
  assert.throws(() => parseDeviceAdd(JSON.stringify({ device: { id: "d1" }, token: "has space here" })));
  assert.throws(() => parseDeviceAdd(JSON.stringify({ token })));
  assert.throws(() => parseDeviceAdd("added device n (d1)"));
});

// ---------------------------------------------------------------------------------------
// Backoff and state

test("reconnects wait 1, 2, 5, 10, then 30 s", () => {
  assert.deepEqual([0, 1, 2, 3, 4, 5, 20].map(backoff), [1000, 2000, 5000, 10000, 30000, 30000, 30000]);
});

test("tunnel state: a revoked tunnel stays stopped until a new connect", () => {
  let s: TunnelState = tunnelReduce(undefined, { type: "connecting" });
  assert.deepEqual(s, { status: "connecting" });
  s = tunnelReduce(s, { type: "up", localPort: 5, remotePort: 7420 });
  assert.deepEqual(s, { status: "connected", localPort: 5, remotePort: 7420 });
  s = tunnelReduce(s, { type: "down", reason: "ssh exited" });
  assert.deepEqual(s, { status: "offline", localPort: 5, remotePort: 7420, reason: "ssh exited" });
  s = tunnelReduce(s, { type: "unauthorized" });
  assert.equal(s.stopped, true);
  assert.equal(s.reason, REVOKED);
  assert.equal(tunnelReduce(s, { type: "down", reason: "x" }), s);
  assert.equal(tunnelReduce(s, { type: "up", localPort: 5, remotePort: 1 }), s);
  assert.equal(tunnelReduce(s, { type: "connecting" }).status, "connecting");
});

// ---------------------------------------------------------------------------------------
// remote.json

/** Stands in for safeStorage: reversible, and visibly not the clear text. */
function fakeCrypto(unavailable?: string): Crypto {
  return {
    unavailable: () => unavailable,
    encrypt: (text) => Buffer.from(`sealed:${Buffer.from(text).toString("hex")}`),
    decrypt: (data) => {
      const s = data.toString();
      if (!s.startsWith("sealed:")) throw new Error("cannot decrypt");
      return Buffer.from(s.slice(7), "hex").toString();
    },
  };
}

test("remote.json never holds the token in the clear, and refuses without encryption", async () => {
  const dir = mkdtempSync(join(tmpdir(), "crossagents-remote-"));
  try {
    const file = join(dir, "remote.json");
    const token = `cad_${"cd".repeat(32)}`;
    const record: RemoteRecord = {
      mode: "remote", target: { host: "center" }, deviceName: "n", deviceId: "d", pairedAt: "2026-11-08T00:00:00Z", localPort: 50001,
      token: sealToken(fakeCrypto(), token),
    };
    await writeRecord(file, record);
    assert.ok(!readFileSync(file, "utf8").includes(token));
    const back = await readRecord(file);
    assert.deepEqual(back, record);
    assert.equal(openToken(fakeCrypto(), back!.token), token);
    assert.equal(openToken({ ...fakeCrypto(), decrypt: () => { throw new Error("other user"); } }, back!.token), undefined);
    assert.throws(() => sealToken(fakeCrypto("basic_text"), token), /basic_text/);
    const state = publicState(back, { status: "connected", localPort: 50001 }, "pc-gui");
    assert.ok(!JSON.stringify(state).includes(token));
    assert.ok(!JSON.stringify(state).includes(back!.token));
    // A record someone edited by hand is not trusted.
    writeFileSync(file, JSON.stringify({ ...record, target: { host: "-oProxyCommand=x" } }));
    assert.equal(await readRecord(file), undefined);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// ---------------------------------------------------------------------------------------
// Pairing and the tunnel, end to end with the fake ssh and a fake hub.

const SSH: Ssh = { program: process.execPath, prefix: [join(__dirname, "fakeSsh.cjs")] };

type FakeHub = { port: number; tokens: Set<string>; close(): Promise<void> };

async function fakeHub(tokens: Set<string>, port = 0): Promise<FakeHub> {
  const server: Server = createServer((req, res) => {
    if (req.url === "/v1/health") return void res.writeHead(200, { "content-type": "application/json" }).end('{"status":"ok","api":"2"}');
    const auth = req.headers.authorization?.replace(/^Bearer /, "") ?? "";
    if (!tokens.has(auth)) return void res.writeHead(401).end('{"error":{"code":"unauthorized","message":"no"}}');
    res.writeHead(200, { "content-type": "application/json" }).end('{"version":"0.2.10"}');
  });
  await new Promise<void>((done) => server.listen(port, "127.0.0.1", done));
  const address = server.address();
  return {
    port: typeof address === "object" && address ? address.port : 0,
    tokens,
    close: () => new Promise((done) => {
      server.closeAllConnections();
      server.close(() => done());
    }),
  };
}

function setEnv(values: Record<string, string | undefined>) {
  for (const key of ["FAKE_SSH_FAIL", "FAKE_SSH_HUB_PORT", "FAKE_SSH_CMD_ONLY", "FAKE_SSH_TOKEN", "FAKE_SSH_PAIR_FAIL", "FAKE_SSH_LOG", "FAKE_SSH_EXE", "FAKE_SSH_PS_VERSION"]) {
    delete process.env[key];
  }
  for (const [key, value] of Object.entries(values)) if (value !== undefined) process.env[key] = value;
}

async function until(what: string, check: () => boolean, ms = 15_000) {
  const deadline = Date.now() + ms;
  while (!check()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

function controller(dir: string, crypto = fakeCrypto()) {
  const states: ConnectionState[] = [];
  const steps: PairStep[] = [];
  const changes: string[] = [];
  const c = new RemoteController({
    file: join(dir, "remote.json"),
    crypto,
    ssh: SSH,
    defaultDeviceName: "pc-gui",
    onState: (s) => states.push(s),
    onPairStep: (s) => steps.push(s),
    onHubChange: (m) => changes.push(m),
    tunnel: { upTimeoutMs: 10_000, watchMs: 100, delay: () => 50 },
  });
  return { c, states, steps, changes };
}

test("locating: ssh's own failure is shown as is; no hub.json means the hub does not run", async () => {
  setEnv({ FAKE_SSH_FAIL: "Host key verification failed." });
  await assert.rejects(locate(SSH, { host: "center" }), (err: Error) => err.message.includes("Host key verification failed."));
  setEnv({});
  await assert.rejects(locate(SSH, { host: "center" }), (err: Error) => err.message.startsWith(NOT_RUNNING));
  setEnv({ FAKE_SSH_HUB_PORT: "7420", FAKE_SSH_CMD_ONLY: "1" });
  assert.deepEqual(await locate(SSH, { host: "center" }), { port: 7420, exe: "C:\\Users\\u\\.crossagents\\bin\\0.2.10\\crossagents.exe", locateMethod: "type" });
  setEnv({});
});

test("pairing probes PowerShell only for Windows paths found with cat", async () => {
  const dir = mkdtempSync(join(tmpdir(), "crossagents-shell-"));
  const log = join(dir, "ssh.log");
  const target = { host: "center" };
  try {
    for (const scenario of [
      { exe: "C:\\Program Files\\crossagents.exe", version: "7", method: "cat", probe: true, prefix: "& " },
      { exe: "\\\\server\\share\\crossagents.exe", version: "5", method: "cat", probe: true, prefix: "& " },
      { exe: "C:\\crossagents.exe", version: undefined, method: "cat", probe: true, prefix: "" },
      { exe: "C:\\crossagents.exe", version: "7", method: "type", probe: false, prefix: "" },
      { exe: "/opt/crossagents", version: "7", method: "cat", probe: false, prefix: "" },
      { exe: "", version: "7", method: "cat", probe: false, prefix: "" },
    ] as const) {
      writeFileSync(log, "");
      setEnv({ FAKE_SSH_HUB_PORT: "7420", FAKE_SSH_EXE: scenario.exe, FAKE_SSH_PS_VERSION: scenario.version, FAKE_SSH_CMD_ONLY: scenario.method === "type" ? "1" : undefined, FAKE_SSH_LOG: log });
      const hub = await locate(SSH, target);
      await pairDevice(SSH, target, hub, "pc-gui");
      const calls = readFileSync(log, "utf8").trim().split("\n").map((line) => JSON.parse(line) as string[]);
      const commands = calls.map((args) => args[args.length - 1]);
      assert.equal(commands.includes("$PSVersionTable.PSVersion.Major"), scenario.probe);
      const executable = scenario.exe ? `"${scenario.exe}"` : "crossagents";
      assert.equal(commands.at(-1), `${scenario.prefix}${executable} device add pc-gui --json`);
    }
  } finally {
    setEnv({});
    rmSync(dir, { recursive: true, force: true });
  }
});

test("pairing, connecting, reconnecting after the hub moves, and stopping on 401", async () => {
  const dir = mkdtempSync(join(tmpdir(), "crossagents-remote-"));
  const token = `cad_${"ef".repeat(32)}`;
  let hub = await fakeHub(new Set([token]));
  const log = join(dir, "ssh.log");
  setEnv({ FAKE_SSH_HUB_PORT: String(hub.port), FAKE_SSH_TOKEN: token, FAKE_SSH_LOG: log });
  const { c, steps, changes } = controller(dir);
  try {
    await c.init();
    assert.equal(c.mode(), "local");
    assert.throws(() => c.hub(), Unavailable);

    const state = await c.pair({ host: "center", user: "u" }, "pc-gui");
    assert.deepEqual(steps, ["locate", "pair", "tunnel", "check"]);
    assert.deepEqual(changes, ["remote"]);
    assert.equal(state.mode, "remote");
    assert.equal(state.tunnel?.status, "connected");
    assert.equal(state.remote?.deviceId, "dev-1");
    assert.equal(state.tunnel?.remotePort, hub.port);
    assert.ok(!JSON.stringify(state).includes(token));
    // The pairing ran the hub's own exe, quoted.
    assert.ok(readFileSync(log, "utf8").includes('"/home/u/.crossagents/bin/0.2.10/crossagents\\" device add pc-gui --json'));
    const first = c.hub();
    assert.equal(first.token, token);
    assert.equal((await fetch(`${first.base}/v1/info`, { headers: { authorization: `Bearer ${token}` } })).status, 200);
    const localPort = state.tunnel!.localPort!;
    const saved = JSON.parse(readFileSync(join(dir, "remote.json"), "utf8"));
    assert.equal(saved.localPort, localPort);
    assert.ok(!JSON.stringify(saved).includes(token));

    // The hub restarts on another port: the tunnel notices, reads hub.json again and keeps its local port.
    await hub.close();
    await until("offline", () => c.get().tunnel?.status === "offline");
    hub = await fakeHub(hub.tokens);
    process.env.FAKE_SSH_HUB_PORT = String(hub.port);
    await until("reconnected", () => c.get().tunnel?.status === "connected");
    assert.equal(c.get().tunnel?.localPort, localPort);
    assert.equal(c.get().tunnel?.remotePort, hub.port);

    // Revoked: a request's 401 stops it, and reconnecting does not help until re-paired.
    hub.tokens.clear();
    c.unauthorized();
    assert.equal(c.get().tunnel?.reason, REVOKED);
    assert.equal(c.get().tunnel?.stopped, true);
    assert.throws(() => c.hub(), (err: Error) => err instanceof Unavailable && err.message.includes(REVOKED));
    await c.reconnect();
    await until("stopped again", () => c.get().tunnel?.stopped === true);
    assert.equal(c.get().tunnel?.reason, REVOKED);

    // Back to local: the tunnel ends and the pairing stays.
    const local = await c.use("local");
    assert.equal(local.mode, "local");
    assert.equal(local.tunnel, undefined);
    assert.equal(local.remote?.deviceId, "dev-1");
    assert.deepEqual(changes, ["remote", "local"]);
    // Forget: gone from disk; nothing was revoked on the hub (no ssh call after the switch).
    const calls = readFileSync(log, "utf8").split("\n").length;
    const forgotten = await c.forget();
    assert.equal(forgotten.remote, undefined);
    assert.equal(await readRecord(join(dir, "remote.json")), undefined);
    assert.equal(readFileSync(log, "utf8").split("\n").length, calls);
  } finally {
    await c.dispose();
    await hub.close();
    setEnv({});
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a failed pairing carries ssh's stderr and saves nothing; no encryption, no pairing", async () => {
  const dir = mkdtempSync(join(tmpdir(), "crossagents-remote-"));
  try {
    setEnv({ FAKE_SSH_HUB_PORT: "7420", FAKE_SSH_PAIR_FAIL: "error: unauthorized: missing or invalid bearer token" });
    const { c } = controller(dir);
    await c.init();
    await assert.rejects(c.pair({ host: "center" }, "pc-gui"), /missing or invalid bearer token/);
    assert.equal(c.get().remote, undefined);
    await assert.rejects(c.pair({ host: "-oProxyCommand=calc" }, "pc-gui"), InputError);

    setEnv({ FAKE_SSH_HUB_PORT: "7420", FAKE_SSH_LOG: join(dir, "ssh.log") });
    const { c: plain } = controller(dir, fakeCrypto("the system keyring is not available (basic_text)"));
    await assert.rejects(plain.pair({ host: "center" }, "pc-gui"), /basic_text/);
    // Refused before any token was issued.
    assert.throws(() => readFileSync(join(dir, "ssh.log")));
    assert.equal(await readRecord(join(dir, "remote.json")), undefined);
  } finally {
    setEnv({});
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a saved token that cannot be decrypted stops at 请重新配对", async () => {
  const dir = mkdtempSync(join(tmpdir(), "crossagents-remote-"));
  try {
    await writeRecord(join(dir, "remote.json"), {
      mode: "remote", target: { host: "center" }, deviceName: "n", deviceId: "d", pairedAt: "x", token: Buffer.from("garbage").toString("base64"),
    });
    const { c } = controller(dir);
    await c.init();
    assert.equal(c.mode(), "remote");
    assert.deepEqual(c.get().tunnel, { status: "offline", reason: REPAIR, stopped: true });
    assert.throws(() => c.hub(), (err: Error) => err instanceof Unavailable && err.message.includes(REPAIR));
    await c.dispose();
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
