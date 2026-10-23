import assert from "node:assert/strict";
import { test } from "node:test";
import { errorHint } from "./errors.ts";

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
