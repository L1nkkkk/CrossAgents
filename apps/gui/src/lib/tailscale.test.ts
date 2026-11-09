import assert from "node:assert/strict";
import { test } from "node:test";
import type { TailscalePeer } from "../bridge";
import { peerAddress, sortPeers, tailscaleNote } from "./tailscale.ts";

const peer = (hostName: string, fields: Partial<TailscalePeer> = {}): TailscalePeer => ({ hostName, ips: ["100.64.0.1"], os: "linux", online: true, ...fields });

test("online peers come first, then by host name", () => {
  const sorted = sortPeers([peer("b", { online: false }), peer("z"), peer("a", { online: false }), peer("m")]);
  assert.deepEqual(
    sorted.map((p) => p.hostName),
    ["m", "z", "a", "b"],
  );
});

test("address prefers the MagicDNS name, then the first IP", () => {
  assert.equal(peerAddress(peer("a", { dnsName: "a.tail.ts.net" })), "a.tail.ts.net");
  assert.equal(peerAddress(peer("a", { dnsName: "  " })), "100.64.0.1");
  assert.equal(peerAddress(peer("a", { ips: [] })), undefined);
});

test("every state but running has a note", () => {
  assert.match(tailscaleNote({ state: "missing", peers: [] })!, /tailscale\.com\/download/);
  assert.match(tailscaleNote({ state: "stopped", peers: [], error: "boom" })!, /boom/);
  assert.match(tailscaleNote({ state: "needs-login", peers: [] })!, /登录/);
  assert.equal(tailscaleNote({ state: "running", peers: [] }), undefined);
});
