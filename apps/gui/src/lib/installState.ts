// What a vendor CLI is doing right now (docs/DESIGN.md §9.3): the hub reports an install or
// upgrade in progress on `GET /v1/plugins` (`CliInfo.installing`) and says so at once on the
// event stream (`cli.install.started` / `cli.install.finished`). The rows read both, so a
// button is disabled for a job this window did not start, and a second click is not sent.

import type { CliInfo } from "@bindings/CliInfo";

/** What the hub runs for a plugin: `install` or `upgrade`. */
export type InstallAction = "install" | "upgrade";

/** A row's own job, as the event stream described it. */
export type RunningJob = { action: string; done?: unknown };

/**
 * What is running for one CLI now: this window's job while it has not finished, else what
 * the hub last reported. `undefined` when nothing runs.
 */
export function installingOf(cli: CliInfo | undefined, job?: RunningJob): InstallAction | undefined {
  if (job && !job.done) return job.action === "upgrade" ? "upgrade" : "install";
  const reported = cli?.installing;
  if (reported === "upgrade") return "upgrade";
  if (reported === "install") return "install";
  return undefined;
}

/** What the running row says: 「安装中」/「升级中」. */
export function installingLabel(action: InstallAction): string {
  return action === "upgrade" ? "升级中" : "安装中";
}

/** Whether the hub refused because that plugin already has an install or upgrade running. */
export function installBusy(err: unknown): boolean {
  const status = (err as { status?: unknown } | null)?.status;
  const message = err instanceof Error ? err.message : String(err);
  return status === 409 && /already being (installed|upgraded)/i.test(message);
}

/**
 * A refused install in words for the person. The hub answers 409 while another install or
 * upgrade for that plugin runs (`crates/hub/src/install.rs` `claim`); that one says to
 * wait, not the hub's own English sentence.
 */
export function installRefusedText(err: unknown, action: InstallAction): string {
  if (installBusy(err)) {
    return action === "upgrade" ? "正在升级，请等待完成" : "正在安装，请等待完成";
  }
  const message = err instanceof Error ? err.message : String(err);
  return message.split("\n")[0];
}
