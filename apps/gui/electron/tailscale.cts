// This machine's Tailscale, for the GUI's connection page (electron/main.cts `tailscale:status`).
// Runs the installed `tailscale status --json` with execFile (never a shell). The parsing is
// pure and tested (tailscale.test.cts); only `tailscaleStatus` touches the system.

import { execFile } from "node:child_process";
import type { TailscaleOs, TailscalePeer, TailscaleStatus } from "../src/bridge";

/** Longer than a local daemon takes to answer; a stuck CLI must not hold the page. */
const TIMEOUT_MS = 5_000;
const MAX_BUFFER = 4 << 20;

/** Where the CLI is, in order: PATH first, then the place the app installs it on each platform. */
export function candidates(platform: NodeJS.Platform): string[] {
  if (platform === "win32") return ["tailscale", "C:\\Program Files\\Tailscale\\tailscale.exe"];
  if (platform === "darwin") return ["tailscale", "/Applications/Tailscale.app/Contents/MacOS/Tailscale"];
  return ["tailscale"];
}

/** Tailscale's `BackendState`: Running and NeedsLogin have their own states; the rest are stopped. */
export function backendState(state: unknown): TailscaleStatus["state"] {
  if (state === "Running") return "running";
  if (state === "NeedsLogin") return "needs-login";
  return "stopped";
}

/** Tailscale's `OS` (`macOS`, `iOS`, `windows`, …) onto the GUI's few names; anything else is `other`. */
export function peerOs(os: unknown): TailscaleOs {
  switch (typeof os === "string" ? os.toLowerCase() : "") {
    case "windows": return "windows";
    case "macos":
    case "darwin": return "macos";
    case "linux": return "linux";
    case "ios": return "ios";
    case "android": return "android";
    default: return "other";
  }
}

/** One node of `Self` or `Peer` in the status JSON; only the fields the page shows. */
type RawNode = {
  HostName?: unknown;
  DNSName?: unknown;
  OS?: unknown;
  TailscaleIPs?: unknown;
  Online?: unknown;
  LastSeen?: unknown;
};

/** Tailscale writes the zero time for "never seen" (an online node has no real last-seen). */
const ZERO_TIME = "0001-01-01";

function node(raw: RawNode): TailscalePeer {
  const dnsName = typeof raw.DNSName === "string" ? raw.DNSName.replace(/\.$/, "") : "";
  const lastSeen = typeof raw.LastSeen === "string" && !raw.LastSeen.startsWith(ZERO_TIME) ? raw.LastSeen : "";
  return {
    hostName: typeof raw.HostName === "string" ? raw.HostName : "",
    ...(dnsName ? { dnsName } : {}),
    ips: Array.isArray(raw.TailscaleIPs) ? raw.TailscaleIPs.filter((ip): ip is string => typeof ip === "string") : [],
    os: peerOs(raw.OS),
    online: raw.Online === true,
    ...(lastSeen ? { lastSeen } : {}),
  };
}

/**
 * `tailscale status --json` text to a status (state from `BackendState`, `Self`, and the peers
 * in `Peer`). Throws when the text is not that JSON.
 */
export function parseStatus(text: string): TailscaleStatus {
  const value = JSON.parse(text.replace(/^\uFEFF/, "").trim()) as {
    BackendState?: unknown;
    Self?: RawNode | null;
    Peer?: Record<string, RawNode | null> | null;
  } | null;
  if (typeof value !== "object" || value === null || !("BackendState" in value)) throw new Error("没有 BackendState");
  const self = value.Self ? node(value.Self) : undefined;
  const peers = Object.values(value.Peer ?? {})
    .filter((raw): raw is RawNode => typeof raw === "object" && raw !== null)
    .map(node);
  return { state: backendState(value.BackendState), ...(self ? { self } : {}), peers };
}

/** What one run of the CLI gave: not installed, or output (and an error when it exited badly). */
export type Run = { kind: "missing" } | { kind: "done"; stdout: string; error?: string };

/**
 * The status for one run. Output that is the status JSON still counts when the CLI also
 * exited badly; otherwise the run is `stopped`, with the reason.
 */
export function statusFromRun(run: Run): TailscaleStatus {
  if (run.kind === "missing") return { state: "missing", peers: [] };
  try {
    const status = parseStatus(run.stdout);
    return run.error ? { ...status, error: run.error } : status;
  } catch (err) {
    const detail = err instanceof Error ? err.message : String(err);
    return { state: "stopped", error: run.error ?? `tailscale status 的输出不是状态 JSON：${detail}`, peers: [] };
  }
}

function runCli(program: string): Promise<Run> {
  return new Promise((done) => {
    execFile(program, ["status", "--json"], { timeout: TIMEOUT_MS, windowsHide: true, maxBuffer: MAX_BUFFER }, (err, stdout, stderr) => {
      const e = err as (NodeJS.ErrnoException & { killed?: boolean }) | null;
      if (e?.code === "ENOENT") return done({ kind: "missing" });
      if (!e) return done({ kind: "done", stdout: String(stdout) });
      const said = String(stderr ?? "").trim();
      const error = e.killed ? `tailscale status 超过 ${TIMEOUT_MS / 1000} 秒没有回应` : said || e.message;
      done({ kind: "done", stdout: String(stdout ?? ""), error });
    });
  });
}

/** This machine's Tailscale now. Never rejects: what goes wrong is reported in the status. */
export async function tailscaleStatus(platform: NodeJS.Platform = process.platform): Promise<TailscaleStatus> {
  for (const program of candidates(platform)) {
    const run = await runCli(program);
    if (run.kind === "missing") continue;
    return statusFromRun(run);
  }
  return statusFromRun({ kind: "missing" });
}
