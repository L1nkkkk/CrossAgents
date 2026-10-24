// Local mode: the GUI installs the hub it ships and keeps it running and up to date
// (docs/MULTI-NODE.md §13). Only a hub started from `<home>/bin/` is ever started, stopped or
// replaced here; a remote hub or one started by hand (e.g. `target/debug`) is left alone.

import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { copyFile, mkdir, readdir, readFile, rename, rm, stat } from "node:fs/promises";
import { join, relative, resolve, isAbsolute } from "node:path";

const EXE = process.platform === "win32" ? "crossagents.exe" : "crossagents";
/** `hub start` waits up to 30 s for the hub, `hub stop` up to 5 s for it to go. */
const CLI_TIMEOUT_MS = 45_000;
/** A failed start is not retried sooner than this, so an offline page does not spawn a hub every 2 s. */
const RETRY_MS = 30_000;
/** Nor is a hub that went away looked at again sooner than this. */
const RECHECK_MS = 5_000;

/** What `hub.json` says about the running hub (crates/hub/src/paths.rs `HubFile`). */
export type HubFile = { pid: number; port: number; version: string; exe?: string };

/** What to do about the hub `hub.json` describes, given the version this GUI ships. */
export type Plan =
  | { action: "start" }
  | { action: "keep" }
  | { action: "replace"; from: string }
  /** Not ours (remote, dev build, an older hub without `exe`): never touched. */
  | { action: "foreign" }
  /** Ours but newer than this GUI (a downgrade): kept; the GUI is the one to update. */
  | { action: "newer"; running: string };

/** `<home>/bin`: one directory per installed version, `<home>/bin/<version>/crossagents(.exe)`. */
export function binRoot(home: string): string {
  return join(home, "bin");
}

const fold = (p: string) => (process.platform === "win32" ? resolve(p).toLowerCase() : resolve(p));

/** Whether `path` lies inside `dir` (case-insensitive on Windows). */
export function isInside(path: string, dir: string): boolean {
  const rel = relative(fold(dir), fold(path));
  return rel !== "" && !rel.startsWith("..") && !isAbsolute(rel);
}

/**
 * Compares `x.y.z[-pre]` versions: negative when `a` is older. `undefined` when either is not
 * one, so an odd version string never leads to replacing a hub.
 */
export function compareVersions(a: string, b: string): number | undefined {
  const parse = (v: string) => {
    const m = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$/.exec(v.trim());
    return m ? { nums: [Number(m[1]), Number(m[2]), Number(m[3])], pre: m[4] } : undefined;
  };
  const x = parse(a);
  const y = parse(b);
  if (!x || !y) return undefined;
  for (let i = 0; i < 3; i++) if (x.nums[i] !== y.nums[i]) return x.nums[i] - y.nums[i];
  // A release is newer than its pre-releases.
  if (x.pre === y.pre) return 0;
  if (x.pre === undefined) return 1;
  if (y.pre === undefined) return -1;
  return x.pre < y.pre ? -1 : 1;
}

/**
 * Decides what to do. `running` is the hub that answered `/v1/info` with `version`, or
 * undefined when none answers. A hub is ours when `hub.json` names an executable under
 * `<home>/bin/`: that is the only signal, so hand-started hubs keep today's behaviour.
 */
export function plan(running: { file: HubFile; version: string } | undefined, ours: string, home: string): Plan {
  if (!running) return { action: "start" };
  const exe = running.file.exe;
  if (!exe || !isInside(exe, binRoot(home))) return { action: "foreign" };
  const order = compareVersions(running.version, ours);
  if (order === 0) return { action: "keep" };
  if (order === undefined) return running.version === ours ? { action: "keep" } : { action: "foreign" };
  return order < 0 ? { action: "replace", from: running.version } : { action: "newer", running: running.version };
}

function run(exe: string, args: string[]): Promise<string> {
  return new Promise((done, fail) => {
    execFile(exe, args, { timeout: CLI_TIMEOUT_MS, windowsHide: true }, (err, stdout, stderr) => {
      if (err) fail(new Error(`${EXE} ${args.join(" ")} failed: ${(stderr || stdout || err.message).trim()}`));
      else done(stdout.trim());
    });
  });
}

/** The version a `crossagents` executable reports (`crossagents 0.1.0`). */
export async function exeVersion(exe: string): Promise<string> {
  const out = await run(exe, ["--version"]);
  const version = out.split(/\s+/).pop();
  if (!version) throw new Error(`unexpected \`${EXE} --version\` output: ${out}`);
  return version;
}

async function sha256(path: string): Promise<string> {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk as Buffer);
  return hash.digest("hex");
}

async function exists(path: string): Promise<boolean> {
  try {
    await stat(path);
    return true;
  } catch {
    return false;
  }
}

/**
 * Copies the shipped executable to `<home>/bin/<version>/` unless an identical copy is there,
 * and returns the copy. The hub, its runtime hosts and their `mcp` children all run from
 * that copy (`current_exe()`), and hosts outlive the hub, so the shipped file stays free for
 * the next GUI update to replace.
 */
export async function install(shipped: string, home: string, version: string): Promise<string> {
  const dir = join(binRoot(home), version);
  const target = join(dir, EXE);
  if ((await exists(target)) && (await sha256(target)) === (await sha256(shipped))) return target;
  await mkdir(dir, { recursive: true });
  const partial = `${target}.partial`;
  await copyFile(shipped, partial);
  try {
    await rename(partial, target);
  } catch (err) {
    // Same version, different build, and the old copy is running: keep using it.
    await rm(partial, { force: true });
    if (await exists(target)) return target;
    throw err;
  }
  return target;
}

/** Whether some process still runs from `dir`. */
async function inUse(dir: string): Promise<boolean> {
  if (process.platform === "win32") {
    // Windows will not delete a running image: trying is the check.
    try {
      await rm(join(dir, EXE), { force: true });
      return false;
    } catch {
      return true;
    }
  }
  const list = await run("ps", ["-axo", "command="]).catch(() => undefined);
  if (list === undefined) return true;
  return list.split("\n").some((line) => line.trim().startsWith(join(dir, EXE)));
}

/**
 * Removes version directories other than `keep` that no process runs from. A directory in
 * use (a runtime host the new hub adopted, its `mcp` child) waits for a later launch.
 */
export async function cleanup(home: string, keep: string[]): Promise<string[]> {
  const root = binRoot(home);
  let names: string[];
  try {
    names = await readdir(root);
  } catch {
    return [];
  }
  const removed: string[] = [];
  for (const name of names) {
    const dir = join(root, name);
    if (keep.some((k) => fold(k) === fold(dir))) continue;
    if (await inUse(dir)) continue;
    try {
      await rm(dir, { recursive: true, force: true });
      removed.push(name);
    } catch {
      // Something inside is still open; next time.
    }
  }
  return removed;
}

async function readHubFile(home: string): Promise<HubFile | undefined> {
  try {
    const value = JSON.parse(await readFile(join(home, "hub.json"), "utf8"));
    return Number.isInteger(value?.port) && typeof value?.version === "string" ? value : undefined;
  } catch {
    return undefined;
  }
}

/** The hub that answers `/v1/info`, if any, with the version it reports. */
async function probe(home: string): Promise<{ file: HubFile; version: string } | undefined> {
  const file = await readHubFile(home);
  if (!file) return undefined;
  try {
    const token = (await readFile(join(home, "token"), "utf8")).trim();
    const response = await fetch(`http://127.0.0.1:${file.port}/v1/info`, {
      headers: { authorization: `Bearer ${token}` },
      signal: AbortSignal.timeout(5_000),
    });
    if (!response.ok) return undefined;
    const info = (await response.json()) as { version?: unknown };
    return typeof info.version === "string" ? { file, version: info.version } : undefined;
  } catch {
    return undefined;
  }
}

export type LocalHub = {
  /** Resolves once the last check settled; never rejects. */
  ready(): Promise<void>;
  /** Checks again (the hub went away); throttled, and at most once per `RETRY_MS` after a failure. */
  recheck(): void;
  /** Why the local hub could not be started, for the offline reason; undefined when fine. */
  problem(): string | undefined;
};

/**
 * Keeps the local hub of `home` running the version shipped at `shipped`. `log` gets one
 * line per decision.
 */
export function manageLocalHub(shipped: string, home: string, log: (line: string) => void): LocalHub {
  let current: Promise<void> = Promise.resolve();
  let running = false;
  let failedAt = 0;
  let checkedAt = 0;
  let problem: string | undefined;

  // The shipped file does not change while the GUI runs: version and copy are done once.
  let installed: Promise<{ version: string; exe: string }> | undefined;
  const once = async () => {
    installed ??= (async () => {
      const version = await exeVersion(shipped);
      return { version, exe: await install(shipped, home, version) };
    })();
    const { version, exe } = await installed.catch((err: unknown) => {
      installed = undefined;
      throw err;
    });
    const hub = await probe(home);
    const decision = plan(hub, version, home);
    log(`local hub: ${decision.action}${hub ? ` (running ${hub.version} from ${hub.file.exe ?? "unknown"})` : ""}, shipped ${version}`);
    if (decision.action === "replace") {
      // Agents in runtime hosts keep running; the new hub adopts them (docs/PROCESS-SUPERVISION.md).
      await run(exe, ["hub", "stop"]);
    }
    if (decision.action === "start" || decision.action === "replace") await run(exe, ["hub", "start"]);
    const now = await readHubFile(home);
    const removed = await cleanup(home, [join(binRoot(home), version), ...(now?.exe ? [join(now.exe, "..")] : [])]);
    if (removed.length) log(`local hub: removed old versions ${removed.join(", ")}`);
  };

  const check = () => {
    if (running) return;
    running = true;
    current = once()
      .then(() => {
        problem = undefined;
        failedAt = 0;
      })
      .catch((err: unknown) => {
        problem = `cannot start the local hub: ${err instanceof Error ? err.message : String(err)}`;
        failedAt = Date.now();
        log(problem);
      })
      .finally(() => {
        running = false;
        checkedAt = Date.now();
      });
  };

  check();
  return {
    ready: () => current,
    recheck: () => {
      const now = Date.now();
      if (now - failedAt >= RETRY_MS && now - checkedAt >= RECHECK_MS) check();
    },
    problem: () => problem,
  };
}
