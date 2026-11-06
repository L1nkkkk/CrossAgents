import assert from "node:assert/strict";
import { test } from "node:test";
import { deliveryFailure, deliveryHint, errorHint } from "./errors.ts";

test("a known error reads as what to do", () => {
  assert.equal(errorHint("cannot start C:\\bin\\claude.exe: program not found"), "找不到 claude.exe 命令：这台电脑没装它的 CLI");
  assert.equal(errorHint("agent error: Not logged in"), "账号没登录或登录已失效");
  assert.equal(errorHint("something odd\nmore"), "something odd");
  assert.equal(
    errorHint("agent error: the proxy http://127.0.0.1:7890 is not usable (connection refused); start it, or set HTTPS_PROXY (or the system proxy) to a working one"),
    "代理 http://127.0.0.1:7890 连不上：先打开代理软件，或改 HTTPS_PROXY / 系统代理",
  );
});

test("an ended process says how it ended", () => {
  const crashed = "claude crashed (exit code 3)\nstderr: boom";
  assert.equal(errorHint(crashed, { cause: "crashed", code: 3 }), "进程崩溃了（退出码 3）");
  assert.equal(errorHint("codex was lost", { cause: "lost", code: null }), "进程失联了：它所在的运行宿主断开了");
  assert.equal(errorHint("claude exited", { cause: "exited", code: 0 }), "进程自己退出了");
  assert.equal(errorHint("claude exited", { cause: null, code: null }), "进程退出了");
  // A known reason still shows after how it ended.
  assert.equal(errorHint("claude crashed (exit code 1)\nError: not logged in", { cause: "crashed", code: 1 }), "进程崩溃了（退出码 1）：账号没登录或登录已失效");
});

test("a failed delivery reads as the reason the hub gave", () => {
  assert.equal(
    deliveryHint({ status: 409, code: "conflict", reason: "already_started", message: "already started" }),
    "这条已经开始，不能再移动或撤回",
  );
  assert.equal(
    deliveryHint({ status: 502, code: "agent_error", reason: "runtime_no_steer", message: "cannot steer" }),
    "这个驱动不能插话，已排到下一轮",
  );
  // A steer write failure is neither sent nor queued: the sentence says to send it again.
  const write = deliveryHint({ status: 500, code: "internal", reason: "steer_write_failed", message: "write failed" });
  assert.match(write, /没插进去/);
  assert.match(write, /重发/);
});

test("a request that got no answer is not reported as a plain failure", () => {
  const unknown = deliveryHint({ status: 0, code: "outcome_unknown", message: "The operation was aborted due to timeout" });
  assert.match(unknown, /不确定发没发出去/);
  assert.match(unknown, /先别重发/);
  const refused = deliveryHint({ status: 0, code: "unreachable", message: "connect ECONNREFUSED" });
  assert.match(refused, /没发出去/);
  assert.doesNotMatch(refused, /不确定/);
});

test("any other delivery error falls back to the agent-error hints, never losing the text", () => {
  assert.equal(deliveryHint({ status: 400, code: "bad_request", message: "the text is too long" }), "the text is too long");
  // A version mismatch explains itself rather than being shortened.
  assert.equal(
    deliveryHint({ status: 0, code: "version_mismatch", message: "hub API v2, GUI API v1: update both to the same version" }),
    "hub API v2, GUI API v1: update both to the same version",
  );
});

test("a thrown error is read structurally, keeping status, code and reason", () => {
  const err = Object.assign(new Error("conflict"), { status: 409, code: "conflict", reason: "archived" });
  assert.deepEqual(deliveryFailure(err), { status: 409, code: "conflict", reason: "archived", message: "conflict" });
  assert.deepEqual(deliveryFailure("boom"), { status: 0, code: "unknown", message: "boom" });
});
