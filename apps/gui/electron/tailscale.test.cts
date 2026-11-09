import assert from "node:assert/strict";
import { test } from "node:test";
import { backendState, candidates, parseStatus, peerOs, statusFromRun } from "./tailscale.cjs";

// A `tailscale status --json` as the CLI writes it (trimmed to the fields the page reads).
const SAMPLE = JSON.stringify({
  Version: "1.80.0-t1234abcd",
  BackendState: "Running",
  Self: {
    ID: "nSelf",
    HostName: "work-pc",
    DNSName: "work-pc.tail1234.ts.net.",
    OS: "windows",
    TailscaleIPs: ["100.64.0.1", "fd7a:115c:a1e0::1"],
    Online: true,
    LastSeen: "0001-01-01T00:00:00Z",
  },
  Peer: {
    "nodekey:mac": {
      ID: "n1",
      HostName: "mac-mini",
      DNSName: "mac-mini.tail1234.ts.net.",
      OS: "macOS",
      TailscaleIPs: ["100.64.0.2"],
      Online: true,
      LastSeen: "0001-01-01T00:00:00Z",
    },
    "nodekey:nas": {
      ID: "n2",
      HostName: "nas",
      DNSName: "nas.tail1234.ts.net.",
      OS: "linux",
      TailscaleIPs: ["100.64.0.3"],
      Online: false,
      LastSeen: "2026-11-01T08:00:00.000000000Z",
    },
    "nodekey:phone": {
      ID: "n3",
      HostName: "iphone",
      DNSName: "iphone.tail1234.ts.net.",
      OS: "iOS",
      TailscaleIPs: ["100.64.0.4"],
      Online: false,
      LastSeen: "2026-10-30T12:00:00Z",
    },
    "nodekey:droid": {
      ID: "n4",
      HostName: "pixel",
      DNSName: "",
      OS: "android",
      TailscaleIPs: [],
      Online: true,
      LastSeen: "0001-01-01T00:00:00Z",
    },
    "nodekey:bsd": {
      ID: "n5",
      HostName: "old-box",
      OS: "freebsd",
      TailscaleIPs: ["100.64.0.6", 7],
      Online: true,
    },
  },
});

test("parses the status JSON: running, self and each peer's OS", () => {
  const status = parseStatus(SAMPLE);
  assert.deepEqual(status, {
    state: "running",
    self: {
      hostName: "work-pc",
      dnsName: "work-pc.tail1234.ts.net",
      ips: ["100.64.0.1", "fd7a:115c:a1e0::1"],
      os: "windows",
      online: true,
    },
    peers: [
      { hostName: "mac-mini", dnsName: "mac-mini.tail1234.ts.net", ips: ["100.64.0.2"], os: "macos", online: true },
      { hostName: "nas", dnsName: "nas.tail1234.ts.net", ips: ["100.64.0.3"], os: "linux", online: false, lastSeen: "2026-11-01T08:00:00.000000000Z" },
      { hostName: "iphone", dnsName: "iphone.tail1234.ts.net", ips: ["100.64.0.4"], os: "ios", online: false, lastSeen: "2026-10-30T12:00:00Z" },
      { hostName: "pixel", ips: [], os: "android", online: true },
      { hostName: "old-box", ips: ["100.64.0.6"], os: "other", online: true },
    ],
  });
});

test("the BOM and surrounding whitespace do not matter", () => {
  assert.equal(parseStatus(`\uFEFF  ${SAMPLE}\n`).state, "running");
});

test("BackendState maps to the four states", () => {
  assert.equal(backendState("Running"), "running");
  assert.equal(backendState("NeedsLogin"), "needs-login");
  assert.equal(backendState("Stopped"), "stopped");
  assert.equal(backendState("NeedsMachineAuth"), "stopped");
  assert.equal(backendState("Starting"), "stopped");
  assert.equal(backendState(undefined), "stopped");
});

test("a status without Self or Peer still parses", () => {
  assert.deepEqual(parseStatus(JSON.stringify({ BackendState: "NeedsLogin" })), { state: "needs-login", peers: [] });
  assert.deepEqual(parseStatus(JSON.stringify({ BackendState: "Stopped", Self: null, Peer: null })), { state: "stopped", peers: [] });
});

test("the output must be the status JSON", () => {
  assert.throws(() => parseStatus("not json"));
  assert.throws(() => parseStatus("null"));
  assert.throws(() => parseStatus("{}"));
});

test("OS names", () => {
  assert.equal(peerOs("windows"), "windows");
  assert.equal(peerOs("macOS"), "macos");
  assert.equal(peerOs("darwin"), "macos");
  assert.equal(peerOs("linux"), "linux");
  assert.equal(peerOs("iOS"), "ios");
  assert.equal(peerOs("android"), "android");
  assert.equal(peerOs("tvOS"), "other");
  assert.equal(peerOs(undefined), "other");
});

test("a missing command is `missing`, with no peers", () => {
  assert.deepEqual(statusFromRun({ kind: "missing" }), { state: "missing", peers: [] });
});

test("output that is the status JSON is used even when the CLI exited badly", () => {
  const status = statusFromRun({ kind: "done", stdout: SAMPLE, error: "exit status 1" });
  assert.equal(status.state, "running");
  assert.equal(status.error, "exit status 1");
});

test("no status JSON gives `stopped` with the CLI's reason", () => {
  assert.deepEqual(
    statusFromRun({ kind: "done", stdout: "", error: "failed to connect to local tailscaled" }),
    { state: "stopped", error: "failed to connect to local tailscaled", peers: [] },
  );
  const garbled = statusFromRun({ kind: "done", stdout: "<html>" });
  assert.equal(garbled.state, "stopped");
  assert.match(garbled.error ?? "", /不是状态 JSON/);
});

test("the candidates: PATH first, then the app's own place", () => {
  assert.deepEqual(candidates("win32"), ["tailscale", "C:\\Program Files\\Tailscale\\tailscale.exe"]);
  assert.deepEqual(candidates("darwin"), ["tailscale", "/Applications/Tailscale.app/Contents/MacOS/Tailscale"]);
  assert.deepEqual(candidates("linux"), ["tailscale"]);
});
