// A stand-in for crossagents-link in link.test.cts, run as `node fakeLink.cjs <arguments>`.
// `redeem` reads the code on stdin and hands out a device; `connect` reads the token on stdin,
// forwards its port to the test's fake hub and prints the events the real one prints. Set
// through the environment it inherits:
//   FAKE_LINK_TOKEN        the token `redeem` hands out; `connect` refuses any other (exit 3)
//   FAKE_LINK_REDEEM_FAIL  `redeem` fails with this on stderr
//   FAKE_LINK_HUB_PORT     the fake hub `connect` forwards to
//   FAKE_LINK_LOG          a file each call's arguments and stdin are appended to (one JSON line)

import { appendFileSync } from "node:fs";
import { connect, createServer } from "node:net";

const args = process.argv.slice(2);
const env = process.env;

function arg(name: string): string | undefined {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
}

function stdinLine(): Promise<string> {
  return new Promise((done) => {
    let text = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk: string) => {
      text += chunk;
      if (text.includes("\n")) done(text.split("\n")[0].trim());
    });
    process.stdin.on("end", () => done(text.trim()));
  });
}

const emit = (event: object) => process.stdout.write(`${JSON.stringify(event)}\n`);

void (async () => {
  const input = await stdinLine();
  if (env.FAKE_LINK_LOG) appendFileSync(env.FAKE_LINK_LOG, `${JSON.stringify({ args, stdin: input.slice(0, 8) })}\n`);
  if (args[0] === "redeem") {
    if (env.FAKE_LINK_REDEEM_FAIL) {
      process.stderr.write(`Error: ${env.FAKE_LINK_REDEEM_FAIL}\n`);
      process.exit(1);
    }
    if (!input.startsWith("ca1-")) {
      process.stderr.write("Error: not a CrossAgents pairing code\n");
      process.exit(1);
    }
    process.stdout.write(`${JSON.stringify({ device: { id: "dev-link", name: arg("--name") }, token: env.FAKE_LINK_TOKEN ?? "cad_link" })}\n`);
    return;
  }
  if (args[0] === "connect") {
    const listen = /^127\.0\.0\.1:(\d+)$/.exec(arg("--listen") ?? "");
    if (!listen) {
      process.stderr.write("Error: bad --listen\n");
      process.exit(2);
    }
    const server = createServer((socket) => {
      if (input !== (env.FAKE_LINK_TOKEN ?? "cad_link")) {
        socket.destroy();
        emit({ event: "unauthorized", error: "revoked" });
        process.exit(3);
      }
      const upstream = connect(Number(env.FAKE_LINK_HUB_PORT), "127.0.0.1");
      socket.pipe(upstream).pipe(socket);
      const close = () => {
        socket.destroy();
        upstream.destroy();
      };
      socket.on("error", close);
      upstream.on("error", close);
    });
    server.listen(Number(listen[1]), "127.0.0.1", () => {
      emit({ event: "listening", port: Number(listen[1]) });
      emit({ event: "connected", direct: false });
      setTimeout(() => emit({ event: "path", direct: true }), 200);
    });
    return;
  }
  process.stderr.write(`Error: unknown command ${args[0]}\n`);
  process.exit(2);
})();
