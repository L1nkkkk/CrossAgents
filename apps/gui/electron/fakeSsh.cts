// A stand-in for ssh in remote.test.cts, run as `node fakeSsh.cjs <ssh arguments>`. It answers
// the commands the GUI sends to the hub's machine and, for `-N -L`, forwards the local port to
// the test's fake hub. What it does is set through the environment it inherits:
//   FAKE_SSH_FAIL      ssh itself fails (exit 255) with this on stderr
//   FAKE_SSH_HUB_PORT  the port in the hub's hub.json; unset: no hub.json there
//   FAKE_SSH_CMD_ONLY  only cmd.exe's `type` finds hub.json (a Windows hub)
//   FAKE_SSH_TOKEN     the token `device add` hands out
//   FAKE_SSH_PAIR_FAIL `device add` fails with this on stderr
//   FAKE_SSH_LOG       a file each call's arguments are appended to (one JSON line)

import { appendFileSync } from "node:fs";
import { connect, createServer } from "node:net";

const args = process.argv.slice(2);
const env = process.env;
if (env.FAKE_SSH_LOG) appendFileSync(env.FAKE_SSH_LOG, `${JSON.stringify(args)}\n`);

function fail(message: string, code: number): never {
  process.stderr.write(`${message}\n`);
  process.exit(code);
}

if (env.FAKE_SSH_FAIL) fail(env.FAKE_SSH_FAIL, 255);
if (!args.includes("BatchMode=yes")) fail("fake ssh: BatchMode missing", 255);
const end = args.indexOf("--");
if (end < 0 || !args[end + 1]) fail("fake ssh: no destination after --", 255);
const command = args.slice(end + 2).join(" ");

if (args.includes("-N")) {
  const spec = args[args.indexOf("-L") + 1] ?? "";
  const m = /^127\.0\.0\.1:(\d+):127\.0\.0\.1:(\d+)$/.exec(spec);
  if (!m) fail(`fake ssh: bad -L ${spec}`, 255);
  const remote = Number(m[2]);
  const server = createServer((socket) => {
    const upstream = connect(remote, "127.0.0.1");
    socket.pipe(upstream).pipe(socket);
    const close = () => {
      socket.destroy();
      upstream.destroy();
    };
    socket.on("error", close);
    upstream.on("error", close);
  });
  server.once("error", (err) => fail(`bind [127.0.0.1]:${m[1]}: ${err.message}\nError: Could not request local forwarding.`, 255));
  server.listen(Number(m[1]), "127.0.0.1");
} else if (command === "cat ~/.crossagents/hub.json" || command.startsWith("type ")) {
  const isType = command.startsWith("type ");
  if (env.FAKE_SSH_CMD_ONLY && !isType) fail("'cat' is not recognized as an internal or external command", 1);
  if (!env.FAKE_SSH_HUB_PORT) fail(isType ? "The system cannot find the file specified." : "cat: /home/u/.crossagents/hub.json: No such file or directory", 1);
  const exe = env.FAKE_SSH_CMD_ONLY ? "C:\\Users\\u\\.crossagents\\bin\\0.2.10\\crossagents.exe" : "/home/u/.crossagents/bin/0.2.10/crossagents";
  process.stdout.write(`${isType ? "\uFEFF" : ""}${JSON.stringify({ pid: 1, port: Number(env.FAKE_SSH_HUB_PORT), version: "0.2.10", started_at: "2026-11-08T00:00:00Z", exe })}\n`);
} else {
  const m = /^(?:"[^"]+"|crossagents) device add ([A-Za-z0-9._-]+) --json$/.exec(command);
  if (!m) fail(`sh: unexpected command: ${command}`, 127);
  if (env.FAKE_SSH_PAIR_FAIL) fail(env.FAKE_SSH_PAIR_FAIL, 1);
  process.stdout.write(`${JSON.stringify({ device: { id: "dev-1", name: m[1], kind: "client", level: "write" }, token: env.FAKE_SSH_TOKEN ?? "cad_fake" })}\n`);
}
