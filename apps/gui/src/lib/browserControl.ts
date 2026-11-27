// Who is operating a browser tab, and for how long (browser.tsx). An agent's tool finishes in a
// few hundred milliseconds — far too fast for a person to reach 接管 — so its「正在操作」bar
// stays for a while after the last request on that tab; a person's taking over lasts until they
// hand it back. Pure, so the timing is tested in browserControl.test.ts.

/** How long an agent's「正在操作」bar stays after its last request on a tab. */
export const AGENT_HOLD_MS = 15_000;

/** A tab's driver: an agent's tool (with the moment its bar goes), or the person who took it over. */
export type Hold = { kind: "agent"; agent: string; until: number } | { kind: "human" };

/**
 * `agent` made a request on the tab at `now`: its bar shows from now until 15 s after this
 * request, itself included (so each request pushes the deadline out). A person's taking over is
 * never overwritten: its requests are refused while they hold the tab.
 */
export function holdAfterRequest(hold: Hold | null, agent: string, now: number): Hold {
  if (hold?.kind === "human") return hold;
  return { kind: "agent", agent, until: now + AGENT_HOLD_MS };
}

/** Whether an agent's hold has run out at `now`; a person's never does. */
export const holdLapsed = (hold: Hold | null, now: number): boolean => hold?.kind === "agent" && now >= hold.until;

/** Whether navigating from `from` to `to` only changes the URL fragment (so no full page load fires). */
export function isHashOnlyChange(from: string, to: string): boolean {
  try {
    const a = new URL(from);
    const b = new URL(to);
    return a.origin === b.origin && a.pathname === b.pathname && a.search === b.search && a.href !== b.href;
  } catch {
    return false;
  }
}

export type BrowserWaitCondition = {
  text?: string;
  textGone?: string;
  selector?: string;
  url?: string;
};

export type BrowserWaitProbeResult = {
  matched: boolean;
  url: string;
  title: string;
};

const isFatalWaitError = (message: string) =>
  message.startsWith("human_control:") ||
  message.includes("无效的 CSS 选择器") ||
  message.includes("浏览器标签已关闭") ||
  message.includes("Chrome 标签页已断开") ||
  message.includes("请先在 Chrome 加载");

/**
 * Polls `probe` from outside the page until `input`'s condition holds (or sleeps `ms` when no
 * condition is specified). Outer-loop polling survives page navigations and lets `onTick` abort
 * immediately when the person takes over the tab.
 */
export async function runBrowserWait(opts: {
  input: Record<string, unknown>;
  probe: (cond: BrowserWaitCondition) => Promise<BrowserWaitProbeResult>;
  onTick?: () => void;
  sleep?: (ms: number) => Promise<void>;
  now?: () => number;
  pollMs?: number;
}): Promise<{ ok: true; waited_ms: number; url: string; title: string }> {
  const { input, probe, onTick } = opts;
  const sleep = opts.sleep ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
  const now = opts.now ?? (() => Date.now());
  const pollMs = opts.pollMs ?? 200;

  const text = typeof input.text === "string" && input.text ? input.text : undefined;
  const textGone = typeof input.text_gone === "string" && input.text_gone ? input.text_gone : undefined;
  const selector = typeof input.selector === "string" && input.selector ? input.selector : undefined;
  const url = typeof input.url === "string" && input.url ? input.url : undefined;
  const hasCondition = Boolean(text || textGone || selector || url);
  const started = now();

  if (!hasCondition) {
    const rawMs = input.ms === undefined ? 1000 : Number(input.ms);
    if (!Number.isFinite(rawMs) || rawMs < 0) throw new Error("browser_wait 的 ms 要是非负数字");
    const ms = Math.min(15_000, Math.max(50, Math.round(rawMs)));
    const end = started + ms;
    while (now() < end) {
      onTick?.();
      await sleep(Math.min(pollMs, Math.max(10, end - now())));
    }
    onTick?.();
    const state = await probe({}).catch(() => ({ matched: true, url: "", title: "" }));
    return { ok: true, waited_ms: Math.max(0, now() - started), url: state.url, title: state.title };
  }

  const rawTimeout = input.timeout_ms === undefined ? 10_000 : Number(input.timeout_ms);
  if (!Number.isFinite(rawTimeout) || rawTimeout <= 0) throw new Error("browser_wait 的 timeout_ms 要是正数");
  const timeoutMs = Math.min(20_000, Math.max(100, Math.round(rawTimeout)));
  const deadline = started + timeoutMs;
  const cond: BrowserWaitCondition = { text, textGone, selector, url };

  for (;;) {
    onTick?.();
    try {
      const state = await probe(cond);
      if (state.matched) {
        return { ok: true, waited_ms: Math.max(0, now() - started), url: state.url, title: state.title };
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      if (isFatalWaitError(message)) throw err;
      // Page may be mid-navigation; retry until deadline.
    }
    const remaining = deadline - now();
    if (remaining <= 0) {
      const desc = [
        text && `text=${JSON.stringify(text)}`,
        textGone && `text_gone=${JSON.stringify(textGone)}`,
        selector && `selector=${JSON.stringify(selector)}`,
        url && `url=${JSON.stringify(url)}`,
      ]
        .filter(Boolean)
        .join("、");
      throw new Error(`等待超时（${timeoutMs}ms）：未满足条件（${desc}），请用 browser_snapshot 查看当前页面`);
    }
    await sleep(Math.min(pollMs, remaining));
  }
}
