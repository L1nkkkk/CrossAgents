"""Live probe of `codex app-server` (docs/research/codex-app-server.md).

Runs against a real Codex login, so it spends a few small turns of quota. Uses
`CODEX_HOME` if set (default: Codex's own default, `~/.codex`); every thread it creates
is archived at the end. Working directories are fresh temp dirs.

    python docs/research/codex-app-server-probe.py [path/to/codex.exe] > probe.log

Writes a JSONL transcript (every message both ways, secrets redacted) next to stdout's
summary: `$TEMP/codex-probe/transcript.jsonl`.
"""

import json
import os
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time

REDACT = {"accountId", "email", "userId", "access_token", "refresh_token", "id_token"}
ROOT = os.path.join(tempfile.gettempdir(), "codex-probe")
TRANSCRIPT = os.path.join(ROOT, "transcript.jsonl")


def redact(value):
    if isinstance(value, dict):
        return {k: ("<redacted>" if k in REDACT else redact(v)) for k, v in value.items()}
    if isinstance(value, list):
        return [redact(v) for v in value]
    return value


class AppServer:
    def __init__(self, exe, cwd, label):
        self.label = label
        self.proc = subprocess.Popen(
            [exe, "app-server"],
            cwd=cwd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        self.next_id = 1
        self.pending = {}
        self.notes = queue.Queue()
        self.lock = threading.Lock()
        self.approval_answer = {"decision": "decline"}
        self.server_requests = []
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._stderr, daemon=True).start()

    def _log(self, direction, msg):
        with open(TRANSCRIPT, "a", encoding="utf-8") as f:
            f.write(json.dumps({"t": round(time.time(), 3), "proc": self.label,
                                "dir": direction, "msg": redact(msg)}) + "\n")

    def _send(self, msg):
        self._log("->", msg)
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def _stderr(self):
        for line in self.proc.stderr:
            self._log("stderr", line.rstrip())

    def _read(self):
        for line in self.proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except ValueError:
                self._log("<-raw", line)
                continue
            self._log("<-", msg)
            if "id" in msg and "method" in msg:
                self._server_request(msg)
            elif "id" in msg:
                slot = self.pending.pop(msg["id"], None)
                if slot:
                    slot.put(msg)
            else:
                self.notes.put(msg)
        self.notes.put({"method": "<eof>"})

    def _server_request(self, msg):
        self.server_requests.append(msg)
        method = msg["method"]
        if method.endswith("requestApproval"):
            result = self.approval_answer
        else:
            self._send({"jsonrpc": "2.0", "id": msg["id"],
                        "error": {"code": -32601, "message": f"probe: {method} unsupported"}})
            return
        self._send({"jsonrpc": "2.0", "id": msg["id"], "result": result})

    def request(self, method, params, timeout=120):
        with self.lock:
            rid = self.next_id
            self.next_id += 1
        slot = queue.Queue()
        self.pending[rid] = slot
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        msg = slot.get(timeout=timeout)
        if "error" in msg:
            raise RuntimeError(f"{method}: {msg['error']}")
        return msg["result"]

    def notify(self, method, params=None):
        msg = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        self._send(msg)

    def wait_for(self, pred, timeout=240):
        """Collects notifications until one matches; returns (match, all_seen)."""
        seen = []
        deadline = time.time() + timeout
        while True:
            left = deadline - time.time()
            if left <= 0:
                raise TimeoutError(f"waited {timeout}s; saw {[n.get('method') for n in seen]}")
            note = self.notes.get(timeout=left)
            seen.append(note)
            if note.get("method") == "<eof>":
                raise RuntimeError("app-server exited")
            if pred(note):
                return note, seen

    def drain(self):
        out = []
        while True:
            try:
                out.append(self.notes.get_nowait())
            except queue.Empty:
                return out

    def kill(self):
        self.proc.kill()
        self.proc.wait(timeout=10)


def turn_done(thread_id):
    return lambda n: n.get("method") == "turn/completed" and n["params"]["threadId"] == thread_id


def text_of(seen, thread_id):
    return "".join(n["params"]["delta"] for n in seen
                   if n.get("method") == "item/agentMessage/delta"
                   and n["params"]["threadId"] == thread_id)


def methods(seen):
    counts = {}
    for n in seen:
        counts[n.get("method")] = counts.get(n.get("method"), 0) + 1
    return counts


def user_text(text):
    return [{"type": "text", "text": text, "text_elements": []}]


def main():
    exe = sys.argv[1] if len(sys.argv) > 1 else shutil.which("codex")
    # Thread ids to archive from earlier, aborted runs (one per line).
    leftovers_file = os.path.join(tempfile.gettempdir(), "codex-probe-leftovers.txt")
    leftovers = []
    if os.path.exists(leftovers_file):
        leftovers = [l.strip() for l in open(leftovers_file, encoding="utf-8") if l.strip()]
    shutil.rmtree(ROOT, ignore_errors=True)
    os.makedirs(ROOT)
    dirs = {name: os.path.join(ROOT, name) for name in ["a", "b", "c", "outside", "mcp"]}
    for d in dirs.values():
        os.makedirs(d)
    # A minimal stdio MCP server with one tool, to check MCP mounting through `config`.
    mcp_script = os.path.join(dirs["mcp"], "echo_mcp.py")
    with open(mcp_script, "w", encoding="utf-8") as f:
        f.write(ECHO_MCP)

    results = {}
    created = []

    class Tracked(list):
        def append(self, tid):
            super().append(tid)
            with open(leftovers_file, "a", encoding="utf-8") as f:
                f.write(tid + "\n")

    created = Tracked()

    def record(key, value):
        results[key] = value
        print(f"== {key}: {json.dumps(value, ensure_ascii=False)[:600]}", flush=True)

    srv = AppServer(exe, dirs["a"], "p1")
    init = srv.request("initialize", {
        "clientInfo": {"name": "crossagents_probe", "title": "CrossAgents probe", "version": "0"},
        "capabilities": {"experimentalApi": False, "requestAttestation": False},
    })
    srv.notify("initialized")
    record("initialize", init)

    models = srv.request("model/list", {})
    record("model/list", [{"id": m.get("id"), "model": m.get("model"),
                           "efforts": [e.get("reasoningEffort") for e in m.get("supportedReasoningEfforts", [])],
                           "default": m.get("isDefault")} for m in models.get("data", [])])
    # The user's config.toml may name a model this account cannot use; always pass one.
    model = next((m["model"] for m in models["data"] if m.get("isDefault")), models["data"][0]["model"])
    created.extend(leftovers)

    # 1. thread/start with cwd, sandbox, approvals, instructions and an MCP server via config.
    start = srv.request("thread/start", {
        "model": model,
        "cwd": dirs["a"],
        "approvalPolicy": "never",
        "sandbox": "workspace-write",
        "developerInstructions": "You are a probe. Keep every answer under ten words.",
        "config": {"mcp_servers": {"probe_echo": {
            "command": sys.executable, "args": [mcp_script], "env": {"PROBE_ENV": "ok"}}}},
    })
    ta = start["thread"]["id"]
    created.append(ta)
    record("thread/start", {k: start[k] for k in ["model", "modelProvider", "cwd", "approvalPolicy",
                                                  "sandbox", "reasoningEffort"]}
           | {"thread.id": ta, "thread.path": start["thread"].get("path"),
              "thread.cwd": start["thread"]["cwd"], "historyMode": start["thread"].get("historyMode"),
              "status": start["thread"]["status"]})

    # 2. A turn that calls the MCP tool and replies.
    t1 = srv.request("turn/start", {
        "threadId": ta, "effort": "low",
        "input": user_text("Call the probe_echo tool with text 'hello', then reply with exactly: PONG"),
    })
    _, seen = srv.wait_for(turn_done(ta))
    done = seen[-1]["params"]["turn"]
    record("turn1", {"turn/start.turn": {k: t1["turn"].get(k) for k in ["id", "status"]},
                     "completed.status": done["status"], "text": text_of(seen, ta),
                     "methods": methods(seen)})
    mcp_items = [n["params"]["item"] for n in seen
                 if n.get("method") == "item/completed" and n["params"]["item"]["type"] == "mcpToolCall"]
    record("mcp_tool_call", mcp_items[:1])
    usage = [n["params"] for n in seen if n.get("method") == "thread/tokenUsage/updated"]
    record("token_usage_sample", usage[-1:] if usage else None)
    limits = [n["params"] for n in seen if n.get("method") == "account/rateLimits/updated"]
    record("rate_limits_notification", limits[-1:] if limits else None)
    turn1_id = done["id"]

    status = srv.request("mcpServerStatus/list", {"threadId": ta, "detail": None})
    record("mcpServerStatus/list", [{"name": s["name"], "runtimeStatus": s.get("runtimeStatus"),
                                     "tools": sorted(s.get("tools", {}).keys())[:5]}
                                    for s in status.get("data", [])])

    # 3. Same process, second thread, other cwd; a file edit to see per-turn diffs.
    tb = srv.request("thread/start", {"model": model, "cwd": dirs["b"], "approvalPolicy": "never",
                                      "sandbox": "workspace-write"})["thread"]["id"]
    created.append(tb)
    srv.request("turn/start", {"threadId": tb, "effort": "low", "input": user_text(
        "Create a file named hello.txt containing the single line 'hi', then reply DONE.")})
    _, seen = srv.wait_for(turn_done(tb))
    diffs = [n["params"] for n in seen if n.get("method") == "turn/diff/updated"]
    changes = [n["params"]["item"] for n in seen
               if n.get("method") == "item/completed" and n["params"]["item"]["type"] == "fileChange"]
    record("thread_b_edit", {"file_exists": os.path.exists(os.path.join(dirs["b"], "hello.txt")),
                             "methods": methods(seen), "diff": diffs[-1:] if diffs else None,
                             "fileChange": changes[:1]})
    loaded = srv.request("thread/loaded/list", {})
    record("thread/loaded/list", loaded)

    # 4. Interrupt a long turn.
    srv.request("turn/start", {"threadId": ta, "effort": "low", "input": user_text(
        "Ignore the length limit. Write the numbers 1 to 400, one per line, as plain text.")})
    started, _ = srv.wait_for(lambda n: n.get("method") == "item/agentMessage/delta"
                              and n["params"]["threadId"] == ta, timeout=120)
    turn_id = started["params"]["turnId"]
    t0 = time.time()
    irq = srv.request("turn/interrupt", {"threadId": ta, "turnId": turn_id})
    _, seen = srv.wait_for(turn_done(ta))
    record("interrupt", {"response": irq, "completed.status": seen[-1]["params"]["turn"]["status"],
                         "latency_s": round(time.time() - t0, 2), "methods": methods(seen)})

    # 5. Approval request: read-only sandbox + on-request approvals; decline.
    tc_start = srv.request("thread/start", {"model": model, "cwd": dirs["c"], "approvalPolicy": "untrusted",
                                            "sandbox": "read-only"})
    tc = tc_start["thread"]["id"]
    created.append(tc)
    before = len(srv.server_requests)
    srv.request("turn/start", {"threadId": tc, "effort": "low", "input": user_text(
        "Run the shell command `echo probe > made.txt` in the current directory, then reply DONE.")})
    _, seen = srv.wait_for(turn_done(tc))
    reqs = srv.server_requests[before:]
    record("approval", {"requests": [{"method": r["method"], "id": r["id"],
                                      "params": {k: r["params"].get(k) for k in
                                                 ["threadId", "turnId", "itemId", "command", "cwd",
                                                  "reason", "kind", "approvalId"]}} for r in reqs],
                        "file_exists": os.path.exists(os.path.join(dirs["c"], "made.txt")),
                        "methods": methods(seen)})

    # 6. Writes outside cwd: workspace-write vs danger-full-access, both with approvals off.
    outside = {}
    for mode in ["workspace-write", "danger-full-access"]:
        tid = srv.request("thread/start", {"model": model, "cwd": dirs["c"], "approvalPolicy": "never",
                                           "sandbox": mode})["thread"]["id"]
        created.append(tid)
        target = os.path.join(dirs["outside"], f"{mode}.txt")
        srv.request("turn/start", {"threadId": tid, "effort": "low", "input": user_text(
            f"Write the text 'x' to the absolute path {target} using a shell command. "
            "Do not ask; if it fails, reply FAILED, else reply OK.")})
        _, seen = srv.wait_for(turn_done(tid))
        outside[mode] = {"written": os.path.exists(target), "text": text_of(seen, tid)}
    record("outside_cwd", outside)

    # 7. Fork thread A through turn 1, and revert a copy.
    fork = srv.request("thread/fork", {"threadId": ta, "lastTurnId": turn1_id})
    tf = fork["thread"]["id"]
    created.append(tf)
    record("thread/fork", {"id": tf, "forkedFromId": fork["thread"].get("forkedFromId"),
                           "turns": len(fork["thread"].get("turns", [])), "cwd": fork["cwd"]})
    turns_before = srv.request("thread/turns/list", {"threadId": ta})
    ids = [t["id"] for t in turns_before["data"]]
    try:
        # Drop the interrupted turn (and anything after it) from A's history.
        last = ids[0]  # descending by default
        rev = srv.request("thread/revert", {"threadId": ta, "beforeTurnId": last})
        turns_after = srv.request("thread/turns/list", {"threadId": ta})
        record("thread/revert", {"before": len(ids), "after": len(turns_after["data"]),
                                 "response_keys": list(rev.keys())})
    except Exception as e:  # noqa: BLE001 - record whatever the server says
        record("thread/revert", {"error": str(e)})

    rl = srv.request("account/rateLimits/read", None)
    record("account/rateLimits/read", rl)

    # 8. Kill the process, start a new one, resume thread A and continue.
    srv.kill()
    srv2 = AppServer(exe, dirs["a"], "p2")
    srv2.request("initialize", {"clientInfo": {"name": "crossagents_probe", "title": None, "version": "0"},
                                "capabilities": {"experimentalApi": False, "requestAttestation": False}})
    srv2.notify("initialized")
    resumed = srv2.request("thread/resume", {"threadId": ta, "excludeTurns": True,
                                             "approvalPolicy": "never", "sandbox": "workspace-write"})
    record("thread/resume", {"id": resumed["thread"]["id"], "cwd": resumed["cwd"],
                             "status": resumed["thread"]["status"], "model": resumed["model"]})
    srv2.request("turn/start", {"threadId": ta, "effort": "low", "input": user_text(
        "What exact word did I ask you to reply with in my first message?")})
    _, seen = srv2.wait_for(turn_done(ta))
    record("after_resume", {"text": text_of(seen, ta), "status": seen[-1]["params"]["turn"]["status"]})

    # Resume with another cwd: is the thread bound to its directory?
    try:
        other = srv2.request("thread/resume", {"threadId": tb, "cwd": dirs["c"], "excludeTurns": True})
        record("resume_other_cwd", {"cwd": other["cwd"], "thread.cwd": other["thread"]["cwd"]})
    except Exception as e:  # noqa: BLE001
        record("resume_other_cwd", {"error": str(e)})

    # Unknown thread id.
    try:
        srv2.request("thread/resume", {"threadId": "00000000-0000-7000-8000-000000000000"})
        record("resume_unknown", "accepted?!")
    except Exception as e:  # noqa: BLE001
        record("resume_unknown", {"error": str(e)})

    for tid in created:
        try:
            srv2.request("thread/archive", {"threadId": tid})
        except Exception as e:  # noqa: BLE001
            print(f"archive {tid}: {e}")
    if os.path.exists(leftovers_file):
        os.remove(leftovers_file)
    srv2.kill()
    with open(os.path.join(ROOT, "results.json"), "w", encoding="utf-8") as f:
        json.dump(redact(results), f, indent=2, ensure_ascii=False)
    print("transcript:", TRANSCRIPT)


def followup(exe):
    """Second round: MCP approval mode, concurrent turns, unsubscribe."""
    root = os.path.join(ROOT, "followup")
    shutil.rmtree(root, ignore_errors=True)
    os.makedirs(root)
    mcp_script = os.path.join(root, "echo_mcp.py")
    with open(mcp_script, "w", encoding="utf-8") as f:
        f.write(ECHO_MCP)
    srv = AppServer(exe, root, "f1")
    srv.request("initialize", {"clientInfo": {"name": "crossagents_probe", "title": None, "version": "0"},
                               "capabilities": {"experimentalApi": False, "requestAttestation": False}})
    srv.notify("initialized")
    models = srv.request("model/list", {})
    model = next((m["model"] for m in models["data"] if m.get("isDefault")), models["data"][0]["model"])
    created = []
    out = {}
    for mode in ["workspace-write", "danger-full-access"]:
        tid = srv.request("thread/start", {
            "model": model, "cwd": root, "approvalPolicy": "never", "sandbox": mode,
            "config": {"mcp_servers": {"probe_echo": {
                "command": sys.executable, "args": [mcp_script], "env": {"PROBE_ENV": "ok"},
                "default_tools_approval_mode": "approve"}}},
        })["thread"]["id"]
        created.append(tid)
        srv.request("turn/start", {"threadId": tid, "effort": "low", "input": user_text(
            "Call the probe_echo tool with text 'hello' and reply with its exact output.")})
        _, seen = srv.wait_for(turn_done(tid))
        calls = [n["params"]["item"] for n in seen if n.get("method") == "item/completed"
                 and n["params"]["item"]["type"] == "mcpToolCall"]
        out[mode] = {"status": [c["status"] for c in calls],
                     "result": calls[0].get("result") if calls else None,
                     "error": calls[0].get("error") if calls else None, "text": text_of(seen, tid)}
    print("== mcp_approve:", json.dumps(out, ensure_ascii=False)[:1500], flush=True)

    # Two turns at once in two threads of one process.
    a, b = created
    t0 = time.time()
    for tid, word in [(a, "ALPHA"), (b, "BRAVO")]:
        srv.request("turn/start", {"threadId": tid, "effort": "low",
                                   "input": user_text(f"Reply with exactly: {word}")})
    done = {}
    while len(done) < 2:
        note, _ = srv.wait_for(lambda n: n.get("method") == "turn/completed")
        p = note["params"]
        done[p["threadId"]] = (p["turn"]["status"], round(time.time() - t0, 2))
    print("== concurrent:", json.dumps(done), flush=True)

    # Unsubscribe: does the thread unload, and can it be resumed in the same process?
    uns = srv.request("thread/unsubscribe", {"threadId": a})
    notes = srv.drain()
    loaded = srv.request("thread/loaded/list", {})
    res = srv.request("thread/resume", {"threadId": a, "excludeTurns": True})
    print("== unsubscribe:", json.dumps({"response": uns, "notes": [n.get("method") for n in notes],
                                         "loaded_after": loaded["data"],
                                         "resume_status": res["thread"]["status"]}), flush=True)
    # A second request on a running turn: turn/start while busy.
    srv.request("turn/start", {"threadId": b, "effort": "low", "input": user_text(
        "Ignore brevity. Write the numbers 1 to 300, one per line.")})
    srv.wait_for(lambda n: n.get("method") == "item/agentMessage/delta" and n["params"]["threadId"] == b)
    try:
        second = srv.request("turn/start", {"threadId": b, "input": user_text("Reply OK")})
        print("== turn_start_while_busy:", json.dumps(second)[:400], flush=True)
    except Exception as e:  # noqa: BLE001
        print("== turn_start_while_busy: error", e, flush=True)
    _, seen = srv.wait_for(turn_done(b), timeout=300)
    print("== busy_turn_end:", seen[-1]["params"]["turn"]["status"],
          [n.get("method") for n in seen if n.get("method", "").startswith("turn/")], flush=True)
    for tid in created:
        srv.request("thread/archive", {"threadId": tid})
    srv.kill()


ECHO_MCP = r'''
import json, os, sys
for line in sys.stdin:
    msg = json.loads(line)
    mid, method = msg.get("id"), msg.get("method")
    if mid is None:
        continue
    if method == "initialize":
        res = {"protocolVersion": msg["params"].get("protocolVersion", "2025-06-18"),
               "capabilities": {"tools": {}}, "serverInfo": {"name": "probe_echo", "version": "0"}}
    elif method == "tools/list":
        res = {"tools": [{"name": "probe_echo", "description": "Echo text back",
                          "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}},
                                          "required": ["text"]}}]}
    elif method == "tools/call":
        text = msg["params"]["arguments"].get("text", "")
        res = {"content": [{"type": "text", "text": f"echo:{text} env:{os.environ.get('PROBE_ENV')}"}]}
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": method}}), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": mid, "result": res}), flush=True)
'''

if __name__ == "__main__":
    if "--followup" in sys.argv:
        sys.argv.remove("--followup")
        os.makedirs(ROOT, exist_ok=True)
        followup(sys.argv[1] if len(sys.argv) > 1 else shutil.which("codex"))
    else:
        main()
