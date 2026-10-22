// Live probe of Claude Code's stream-json control protocol (what the Agent SDK speaks).
// Usage: node claude-stream-json-probe.mjs <claude.exe> <work dir> <log.jsonl>
// Needs a signed-in Claude Code. Uses the haiku model to keep the cost down.
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import readline from "node:readline";

const [claude, work, logPath] = process.argv.slice(2);
fs.mkdirSync(work, { recursive: true });
const log = fs.createWriteStream(logPath);
const note = (...a) => console.log("##", ...a);

/** One claude process: send user messages and control requests, await results. */
function start(extra = [], cwd = work) {
  const args = ["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
    "--permission-prompt-tool", "stdio", "--model", "haiku", "--replay-user-messages", ...extra];
  const child = spawn(claude, args, { cwd, env: { ...process.env, CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING: "1" } });
  const pending = new Map();
  const waiters = [];
  const s = { child, sessionId: null, onPermission: () => ({ behavior: "allow" }), seen: [] };
  child.stderr.on("data", (d) => log.write(JSON.stringify({ stderr: String(d) }) + "\n"));
  readline.createInterface({ input: child.stdout }).on("line", (line) => {
    log.write(line + "\n");
    let m;
    try { m = JSON.parse(line); } catch { return; }
    s.seen.push(m);
    if (m.session_id) s.sessionId = m.session_id;
    if (m.type === "control_response") {
      const r = m.response;
      pending.get(r.request_id)?.(r);
      pending.delete(r.request_id);
    } else if (m.type === "control_request" && m.request.subtype === "can_use_tool") {
      const decision = s.onPermission(m.request);
      note("can_use_tool", m.request.tool_name, JSON.stringify(m.request.input).slice(0, 120), "->", decision.behavior);
      const response = decision.behavior === "allow" ? { behavior: "allow", updatedInput: m.request.input } : decision;
      send({ type: "control_response", response: { subtype: "success", request_id: m.request_id, response } });
    } else if (m.type === "control_request") {
      note("other control_request", m.request.subtype);
    }
    for (const w of [...waiters]) if (w.test(m)) { waiters.splice(waiters.indexOf(w), 1); w.resolve(m); }
  });
  const send = (obj) => child.stdin.write(JSON.stringify(obj) + "\n");
  s.wait = (test, ms = 180_000) => new Promise((resolve, reject) => {
    waiters.push({ test, resolve });
    setTimeout(() => reject(new Error("timeout")), ms).unref();
  });
  s.control = (request) => {
    const id = randomUUID();
    send({ type: "control_request", request_id: id, request });
    return new Promise((resolve) => pending.set(id, resolve));
  };
  s.say = (text) => {
    const uuid = randomUUID();
    send({ type: "user", uuid, session_id: s.sessionId ?? "", parent_tool_use_id: null, message: { role: "user", content: text } });
    return uuid;
  };
  s.turn = async (text) => {
    const uuid = s.say(text);
    const result = await s.wait((m) => m.type === "result");
    note("result", result.subtype, "is_error", result.is_error, "text:", String(result.result).slice(0, 160).replace(/\n/g, " "));
    note("usage", JSON.stringify(result.usage), "cost", result.total_cost_usd);
    return { uuid, result };
  };
  s.close = () => new Promise((resolve) => { child.on("exit", resolve); child.stdin.end(); });
  return s;
}

// Follow-up: node claude-stream-json-probe.mjs <claude.exe> <work dir> <log> --followup <first run's log>
// Refusing a shell delete, forking at an assistant message, and what resuming elsewhere remembers.
if (process.argv[5] === "--followup") {
  const first = fs.readFileSync(process.argv[6], "utf8").split("\n").filter(Boolean).flatMap((l) => { try { return [JSON.parse(l)]; } catch { return []; } });
  const sid = first.find((m) => m.session_id).session_id;
  const forkAt = first.find((m) => m.type === "assistant" && JSON.stringify(m.message.content).includes("DONE")).uuid;
  const f = path.join(work, "keep.txt");
  fs.writeFileSync(f, "keep");
  let p = start();
  p.onPermission = (r) => (/PowerShell|Bash/.test(r.tool_name) && /rm |del |Remove-Item/i.test(JSON.stringify(r.input)) ? { behavior: "deny", message: "Not allowed by the hub." } : { behavior: "allow" });
  await p.turn("Delete keep.txt with a shell command, then reply with what happened.");
  note("keep.txt after denied delete:", fs.existsSync(f) ? "still there" : "deleted");
  note("denial in stream", JSON.stringify(p.seen.filter((m) => m.type === "user" && JSON.stringify(m).includes("Not allowed by the hub")).map((m) => m.message.content)).slice(0, 300));
  await p.close();
  p = start(["--resume", sid, "--fork-session", "--resume-session-at", forkAt]);
  await p.turn("Without using any tools: did I ask you to change note.txt to v2 in this conversation? Answer yes or no.");
  note("fork at assistant", forkAt, "new id", p.sessionId, "differs:", p.sessionId !== sid);
  await p.close();
  const other = path.join(work, "..", "cc-other2");
  fs.mkdirSync(other, { recursive: true });
  p = start(["--resume", sid], other);
  await p.turn("Without using any tools: what exact text did I first ask you to put in note.txt, and what is your current working directory? Answer briefly.");
  note("other cwd init cwd", p.seen.find((m) => m.subtype === "init")?.cwd, "session", p.sessionId);
  await p.close();
  log.end();
  process.exit(0);
}

const types = (msgs) => [...new Set(msgs.map((m) => m.type + (m.subtype ? "/" + m.subtype : "")))].join(" ");
const file = path.join(work, "note.txt");
const read = () => (fs.existsSync(file) ? fs.readFileSync(file, "utf8").trim() : "<missing>");

// 1. Handshake and a turn with tools: Bash allowed, Write allowed.
let s = start();
const init = await s.control({ subtype: "initialize" });
note("initialize", init.subtype, Object.keys(init.response ?? {}).join(","));
const t1 = await s.turn("Run the shell command `echo hello-probe`, then create the file note.txt containing exactly v1. Reply DONE.");
note("t1 message types", types(s.seen));
note("note.txt after t1:", read());
const sessionId = s.sessionId;
note("session id", sessionId);

// 2. Edit allowed, a destructive command denied.
s.onPermission = (r) => (r.tool_name === "Bash" && /rm|del|Remove-Item/i.test(JSON.stringify(r.input)) ? { behavior: "deny", message: "Not allowed by the hub." } : { behavior: "allow" });
const t2 = await s.turn("Change note.txt so it contains exactly v2. Then delete note.txt with a shell command. Reply with what happened.");
note("note.txt after t2:", read());

// 3. Rewind files to before turn 2.
note("rewind dry_run", JSON.stringify(await s.control({ subtype: "rewind_files", user_message_id: t2.uuid, dry_run: true })).slice(0, 400));
note("rewind", JSON.stringify(await s.control({ subtype: "rewind_files", user_message_id: t2.uuid })).slice(0, 400));
note("note.txt after rewind:", read());

// 4. Usage and quota.
note("get_usage", JSON.stringify(await s.control({ subtype: "get_usage", skip_behaviors: true })));
note("get_context_usage", JSON.stringify(await s.control({ subtype: "get_context_usage" })).slice(0, 600));
note("rate_limit_event", JSON.stringify(s.seen.filter((m) => m.type === "rate_limit_event").at(-1)));

// 5. Interrupt a long turn.
s.onPermission = () => ({ behavior: "allow" });
const before = Date.now();
s.say("Run the shell command `ping -n 40 127.0.0.1` and report its summary.");
await s.wait((m) => m.type === "assistant" && JSON.stringify(m.message.content).includes("tool_use"));
await new Promise((r) => setTimeout(r, 3000));
const intr = await s.control({ subtype: "interrupt" });
const r5 = await s.wait((m) => m.type === "result");
note("interrupt", intr.subtype, "result", r5.subtype, "after ms", Date.now() - before);

// 6. Model switch.
note("set_model", JSON.stringify(await s.control({ subtype: "set_model", model: "sonnet" })));
await s.close();

// 7. Resume in a new process.
s = start(["--resume", sessionId]);
await s.turn("What exact text did I first ask you to put in note.txt? Answer with just that text.");
note("resumed session id", s.sessionId, "same:", s.sessionId === sessionId);
await s.close();

// 8. Fork at turn 1.
s = start(["--resume", sessionId, "--fork-session", "--resume-session-at", t1.uuid]);
await s.turn("Did I ask you to change note.txt to v2? Answer yes or no.");
note("forked session id", s.sessionId, "differs:", s.sessionId !== sessionId);
await s.close();

// 9. Resume from another directory.
const other = path.join(work, "..", "cc-other");
fs.mkdirSync(other, { recursive: true });
s = start(["--resume", sessionId], other);
try {
  await s.turn("Reply with the word PONG.");
} catch (e) {
  note("other cwd resume failed", e.message);
}
note("other cwd resume last types", types(s.seen.slice(-5)));
s.child.kill();
log.end();
