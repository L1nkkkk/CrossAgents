import assert from "node:assert/strict";
import { test } from "node:test";
import type { ConnectionState, TunnelState } from "../bridge";
import { connectionWords, markWords, PAIR_STEPS, pairFailure, stepWords, targetWords, tunnelWords } from "./connection.ts";

const target = { host: "centre.example", user: "me" };

test("a target reads as user@host, with a non-default port for display", () => {
  assert.equal(targetWords({ host: "centre" }), "centre");
  assert.equal(targetWords({ host: "centre", user: "me" }), "me@centre");
  assert.equal(targetWords({ host: "centre", user: "me", sshPort: 22 }), "me@centre");
  assert.equal(targetWords({ host: "centre", sshPort: 2222 }), "centre:2222");
});

test("the tunnel says 连接中 / 已连接 / 未连接 with ports or the reason", () => {
  assert.deepEqual(tunnelWords(undefined), { word: "未连接", detail: "还没有隧道", tone: "muted" });
  assert.deepEqual(tunnelWords({ status: "connecting" }), { word: "连接中", detail: undefined, tone: "warn" });

  const up = tunnelWords({ status: "connected", localPort: 50123, remotePort: 7420 });
  assert.equal(up.word, "已连接");
  assert.equal(up.detail, "本机 50123 → 中心 7420");
  assert.equal(up.tone, "ok");

  const down = tunnelWords({ status: "offline", reason: "ssh 退出了（退出码 255）", localPort: 50123, remotePort: 7420 });
  assert.equal(down.word, "未连接");
  assert.equal(down.detail, "ssh 退出了（退出码 255）");
  assert.equal(down.tone, "danger");

  // A stop is final: say so, or the person waits for a retry that never comes.
  const stopped = tunnelWords({ status: "offline", reason: "这台设备已被吊销或 token 失效，请重新配对", stopped: true } satisfies TunnelState);
  assert.equal(stopped.detail, "这台设备已被吊销或 token 失效，请重新配对；不会再自动重连");
});

test("local mode is 本机; remote mode carries the target and the tunnel's state", () => {
  assert.deepEqual(connectionWords(undefined), { mode: "local", label: "本机", status: "本机的 hub", tone: "ok" });
  const local: ConnectionState = { mode: "local", defaultDeviceName: "pc-gui" };
  assert.equal(connectionWords(local).label, "本机");

  const remote: ConnectionState = {
    mode: "remote",
    remote: { target, deviceName: "mac-gui", deviceId: "d1", pairedAt: "2026-11-08T10:00:00Z" },
    tunnel: { status: "connected", localPort: 50123, remotePort: 7420 },
    defaultDeviceName: "pc-gui",
  };
  assert.deepEqual(connectionWords(remote), {
    mode: "remote",
    label: "远程 me@centre.example",
    status: "已连接",
    detail: "本机 50123 → 中心 7420",
    tone: "ok",
  });
});

test("the sidebar mark is red whenever the remote tunnel is not up", () => {
  assert.deepEqual(markWords(undefined), {
    text: "本机",
    tone: "ok",
    title: "这个窗口连的是本机的 hub；在设置 →「连接」里可以连到远处的中心",
  });
  const base: ConnectionState = {
    mode: "remote",
    remote: { target, deviceName: "mac-gui", deviceId: "d1", pairedAt: "2026-11-08T10:00:00Z" },
    defaultDeviceName: "pc-gui",
  };
  const up = markWords({ ...base, tunnel: { status: "connected", localPort: 50123, remotePort: 7420 } });
  assert.equal(up.text, "远程 me@centre.example");
  assert.equal(up.tone, "ok");
  // A pairing saved but not started yet is still not connected, and reads red.
  const noTunnel = markWords(base);
  assert.equal(noTunnel.text, "远程 me@centre.example · 未连接");
  assert.equal(noTunnel.tone, "danger");
  // Connecting is not connected: the person should see the difference.
  assert.equal(markWords({ ...base, tunnel: { status: "connecting" } }).text, "远程 me@centre.example · 连接中");
  const down = markWords({ ...base, tunnel: { status: "offline", reason: "隧道断开了" } });
  assert.equal(down.text, "远程 me@centre.example · 未连接");
  assert.equal(down.tone, "danger");
  assert.match(down.title, /隧道断开了/);
});

test("the four pairing steps are named and counted", () => {
  assert.deepEqual(PAIR_STEPS.map((s) => s.key), ["locate", "pair", "tunnel", "check"]);
  assert.equal(stepWords(undefined), "");
  assert.equal(stepWords("locate"), "第 1 步，共 4 步：定位中心的 hub");
  assert.equal(stepWords("check"), "第 4 步，共 4 步：检查设备 token");
});

test("a host key or a password is settled in a terminal first", () => {
  const hostKey = pairFailure("ssh 失败（退出码 255）\nHost key verification failed.", target);
  assert.equal(hostKey.kind, "host-key");
  assert.match(hostKey.next, /先在终端里运行一次 ssh me@centre\.example/);

  const auth = pairFailure("Permission denied (publickey,password).", target);
  assert.equal(auth.kind, "auth");
  assert.match(auth.next, /ssh me@centre\.example/);
  // Without a target the words still say what to run.
  assert.match(pairFailure("Permission denied (publickey).").next, /ssh user@host/);
});

test("a non-default port is ssh -p, never host:port", () => {
  const on2222 = { host: "centre.example", user: "me", sshPort: 2222 };
  // The label may read `:2222`, but a command ssh would accept must use -p.
  for (const reason of [
    "Host key verification failed.",
    "Permission denied (publickey,password).",
    "ssh: connect to host centre.example port 2222: Connection refused",
  ]) {
    const failure = pairFailure(reason, on2222);
    assert.match(failure.next, /ssh -p 2222 me@centre\.example/);
    assert.doesNotMatch(failure.next, /ssh me@centre\.example:2222/);
  }
});

test("a missing crossagents points at PATH or the advanced path", () => {
  for (const reason of [
    "bash: line 1: crossagents: command not found",
    "'crossagents' is not recognized as an internal or external command",
    "The term 'crossagents' is not recognized as the name of a cmdlet, function, script file, or operable program.",
    "fish: Unknown command: crossagents",
    "sh: crossagents: not found",
  ]) {
    const failure = pairFailure(reason, target);
    assert.equal(failure.kind, "no-command", reason);
    assert.match(failure.next, /PATH/);
    assert.match(failure.next, /完整路径/);
  }
});

test("a hub that is not running is started on the centre first", () => {
  const failure = pairFailure("中心上的 hub 没在运行（或不在默认位置，可在高级里填命令）\ncat: /home/me/.crossagents/hub.json: No such file or directory");
  assert.equal(failure.kind, "hub-down");
  assert.match(failure.next, /hub start/);
});

test("network and missing-ssh failures say what to check, and anything else keeps ssh's words", () => {
  assert.equal(pairFailure("ssh: connect to host centre port 22: Connection refused").kind, "network");
  assert.equal(pairFailure("找不到 ssh（ssh.exe）").kind, "ssh-missing");
  assert.equal(pairFailure("ssh 超时").kind, "timeout");
  const other = pairFailure("device add 的输出不是 JSON");
  assert.equal(other.kind, "other");
  assert.match(other.next, /ssh user@host/);
});
