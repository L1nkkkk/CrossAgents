// Agent errors as a person reads them: the hub's messages are for logs, these say what to do.

import type { AgentExit } from "@bindings/AgentExit";
import type { QueuedReason } from "@bindings/QueuedReason";
import { isOutcome, outcomeText, reasonText } from "./delivery.ts";

/**
 * A short Chinese hint for a known error, else its first line. `exit` is how the agent's
 * process ended, when that is the error (`agent.exited`): it leads the hint.
 */
export function errorHint(message: string, exit?: AgentExit | null): string {
  const text = message.replace(/^agent error:\s*/i, "").trim();
  const known = knownHint(text);
  if (exit) {
    const how = exitHint(exit);
    return known ? `${how}：${known}` : how;
  }
  if (known) return known;
  const first = text.split("\n")[0];
  return first.length > 60 ? first.slice(0, 60) + "…" : first;
}

/** How the process ended, in words: "进程崩溃了（退出码 3）". */
export function exitHint({ cause, code }: AgentExit): string {
  const how =
    cause === "crashed" ? "进程崩溃了"
    : cause === "killed" ? "进程被停止了"
    : cause === "lost" ? "进程失联了：它所在的运行宿主断开了"
    : cause === "exited" ? "进程自己退出了"
    : "进程退出了";
  return code != null && code !== 0 ? `${how}（退出码 ${code}）` : how;
}

/** A failed delivery, as the API client knows it: an HTTP status, a code, and the hub's words. */
export type DeliveryFailure = {
  /** 0 when the hub never answered. */
  status: number;
  code: string;
  /** Why the hub refused it, when the error is about delivering a message. */
  reason?: QueuedReason;
  message: string;
};

/**
 * A failed delivery in the person's words (docs/tasks/DELIVERY-AUDIT.md §3, §8 P2-6): the hub's
 * reason code first, then the transport outcome, then the hub's own message run through the
 * agent-error hints. The raw text is for the `title`, never thrown away.
 */
export function deliveryHint(failure: DeliveryFailure): string {
  if (failure.reason) return reasonText(failure.reason);
  const code = failure.code;
  if (isOutcome(code)) return outcomeText(code);
  if (code === "version_mismatch") return failure.message;
  return errorHint(failure.message);
}

/** Reads a thrown API error structurally, so this module stays free of the client (api.ts). */
export function deliveryFailure(err: unknown): DeliveryFailure {
  const e = err as { status?: unknown; code?: unknown; reason?: unknown; message?: unknown };
  return {
    status: typeof e?.status === "number" ? e.status : 0,
    code: typeof e?.code === "string" ? e.code : "unknown",
    ...(typeof e?.reason === "string" ? { reason: e.reason as QueuedReason } : {}),
    message: typeof e?.message === "string" ? e.message : String(err),
  };
}

function knownHint(text: string): string | undefined {
  const missing = /cannot start (.+?): (?:program not found|.*cannot find the file|.*No such file)/i.exec(text);
  if (missing) {
    const program = missing[1].split(/[\\/]/).pop() ?? missing[1];
    return `找不到 ${program} 命令：这台电脑没装它的 CLI`;
  }
  if (/CLI is not installed/i.test(text)) return "这台电脑没装它的 CLI";
  // The driver probes the proxy before starting a CLI (crates/drivers/src/process.rs `check_proxy`).
  const proxy = /the proxy (\S+) is not usable/i.exec(text);
  if (proxy) return `代理 ${proxy[1]} 连不上：先打开代理软件，或改 HTTPS_PROXY / 系统代理`;
  if (/not logged in|login required|please (run )?\/?login|unauthorized|401/i.test(text)) return "账号没登录或登录已失效";
  return undefined;
}
