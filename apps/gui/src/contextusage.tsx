// The context meter is a button: it opens what the window holds in words, and 压缩上下文 —
// which never depends on a threshold, so the person can always find it (the header keeps a
// conspicuous button once it is over half full). The same popup serves the topbar meter and
// the info rail's 上下文 card; the agent's own status says why it cannot run right now, and a
// failure is the hub's own sentence, not a silent no-op.

import { useEffect, useRef, useState } from "react";
import type { Agent } from "@bindings/Agent";
import type { TranscriptUsage } from "@bindings/TranscriptUsage";
import { api, errorText } from "./api";
import { Icon, displayName } from "./components";

/** Why 压缩上下文 cannot start now, in the words on screen; undefined when it can. */
export function compactBlocked(agent: Agent): string | undefined {
  if (agent.status === "running") return "本轮结束后才能压缩";
  if (agent.status === "starting") return "正在启动，稍后再压缩";
  return undefined;
}

/** Compacting a conversation: busy while the hub works, and its own words when it refuses. */
export function useCompact(agent: Agent) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const blocked = compactBlocked(agent);

  async function compact() {
    if (blocked || busy) return;
    setBusy(true);
    setError(undefined);
    try {
      await api.compactAgent(agent.id);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  return { busy, error, blocked, compact };
}

const short = (n: number) => (n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1000 ? `${(n / 1000).toFixed(1)}k` : `${n}`);
const pctOf = (u: TranscriptUsage) => Math.min(100, Math.round((u.used / u.size) * 100));

/**
 * The entry to context usage and compacting: the meter in the topbar (`variant="topbar"`), or
 * the 上下文 card's first row in the rail. With no usage yet it is still an entry — the popup
 * says so and still offers to compact.
 */
export function ContextUsageButton({ agent, usage, variant }: { agent: Agent; usage: TranscriptUsage | null; variant: "topbar" | "rail" }) {
  const [open, setOpen] = useState(false);
  // Fixed to the window, not to the box: the rail scrolls and would clip it.
  const [place, setPlace] = useState<{ right: number; top?: number; bottom?: number }>();
  const ref = useRef<HTMLDivElement>(null);
  const compact = useCompact(agent);
  const has = !!usage && usage.size > 0;
  const pct = has ? pctOf(usage) : 0;

  const anchor = () => {
    const el = ref.current?.querySelector("button");
    if (!el) return;
    const r = el.getBoundingClientRect();
    const right = Math.max(8, window.innerWidth - r.right);
    // Below the trigger unless that would run off the bottom and there is room above.
    const up = r.bottom + 360 > window.innerHeight && r.top > 360;
    setPlace(up ? { right, bottom: window.innerHeight - r.top + 6 } : { right, top: r.bottom + 6 });
  };

  useEffect(() => {
    if (!open) return;
    anchor();
    const close = (e: MouseEvent) => {
      const menu = document.querySelector(".ctx-menu");
      if (ref.current?.contains(e.target as Node) || menu?.contains(e.target as Node)) return;
      setOpen(false);
    };
    const escape = (e: globalThis.KeyboardEvent) => e.key === "Escape" && setOpen(false);
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", escape);
    window.addEventListener("resize", anchor);
    window.addEventListener("scroll", anchor, true);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", escape);
      window.removeEventListener("resize", anchor);
      window.removeEventListener("scroll", anchor, true);
    };
  }, [open]);

  return (
    // Not the bare variant name as a class: `rail` is the info rail's own class (absolute, top
    // right of the pane), and the button would leave its card for the rail's corner.
    <div className={`ctx-pop ${variant === "rail" ? "ctx-pop-rail" : "topbar"}`} ref={ref}>
      {variant === "topbar" ? (
        has ? (
          <button className={`meter-btn ${open ? "open" : ""} ${pct >= 50 ? "attention" : ""}`} onClick={() => setOpen(!open)} aria-haspopup="dialog" aria-expanded={open} title="上下文用量与压缩上下文">
            <Meter usage={usage} />
            <Icon name="chevron" size={12} />
          </button>
        ) : (
          <button className={`ghost small ${open ? "on" : ""}`} onClick={() => setOpen(!open)} aria-haspopup="dialog" aria-expanded={open} title="上下文用量与压缩上下文">
            <span className="row" style={{ gap: 5 }}>
              <Icon name="sliders" size={13} />
              上下文
            </span>
          </button>
        )
      ) : (
        <button className={`rail-context ctx-rail-btn ${open ? "open" : ""}`} onClick={() => setOpen(!open)} aria-haspopup="dialog" aria-expanded={open} title="上下文用量与压缩上下文">
          {has ? (
            <>
              <span className="rail-main">{pct}% 已用</span>
              <span className="rail-dim">
                {short(usage.used)} / {short(usage.size)}
              </span>
            </>
          ) : (
            <span className="rail-main muted">还没有报告用量</span>
          )}
          <Icon name="chevron" size={12} />
        </button>
      )}
      {open && place && <ContextPop agent={agent} usage={has ? usage : null} compact={compact} place={place} />}
    </div>
  );
}

function ContextPop({ agent, usage, compact, place }: { agent: Agent; usage: TranscriptUsage | null; compact: ReturnType<typeof useCompact>; place: { right: number; top?: number; bottom?: number } }) {
  const pct = usage ? pctOf(usage) : 0;
  const t = usage?.tokens;
  const rows: [string, string][] = usage
    ? [
        ...(t
          ? ([
              ["本会话累计", short(t.total)],
              ["输入", `${short(t.input)}（缓存命中 ${t.input ? Math.round((t.cached_input / t.input) * 100) : 0}%）`],
              ["输出", `${short(t.output)}（推理 ${short(t.reasoning_output)}）`],
            ] as [string, string][])
          : []),
        ...(usage.cost_usd != null ? ([["花费", `$${usage.cost_usd.toFixed(4)}`]] as [string, string][]) : []),
      ]
    : [];
  const why = compact.busy ? undefined : compact.blocked;

  return (
    <div className="menu ctx-menu" role="dialog" aria-label="上下文用量与压缩" style={{ position: "fixed", ...place }}>
      {usage ? (
        <>
          <div className="ctx-menu-head">
            <strong>{pct}% 已用</strong>
            <span className="muted small">
              {short(usage.used)} / {short(usage.size)} tokens
            </span>
          </div>
          <div className={`ctx-menu-bar ${pct >= 85 ? "danger" : pct >= 60 ? "warn" : "ok"}`} role="progressbar" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} aria-label="上下文已用">
            <span style={{ width: `${pct}%` }} />
          </div>
          {rows.length > 0 && (
            <dl className="ctx-menu-rows">
              {rows.map(([k, v]) => (
                <div key={k}>
                  <dt>{k}</dt>
                  <dd>{v}</dd>
                </div>
              ))}
            </dl>
          )}
        </>
      ) : (
        <p className="ctx-menu-empty">还没有报告用量：{displayName(agent)} 跑完一轮后才有上下文数据。</p>
      )}
      <div className="ctx-menu-act">
        <button className="small" disabled={compact.busy || !!compact.blocked} onClick={() => void compact.compact()} title={compact.blocked ?? "让 agent 把历史压缩成摘要，腾出上下文"}>
          {compact.busy ? "压缩中…" : "压缩上下文"}
        </button>
        {why && <span className="ctx-why">{why}</span>}
        {compact.error && (
          <p className="ctx-error" role="alert" title={compact.error}>
            {compact.error}
          </p>
        )}
      </div>
    </div>
  );
}

/** How full the context is, the same bar as in the header before it became a button. */
function Meter({ usage }: { usage: TranscriptUsage }) {
  const pct = pctOf(usage);
  return (
    <span className="meter">
      <span className="track">
        <span className={`fill ${pct >= 80 ? "warn" : ""}`} style={{ width: `${pct}%`, display: "block" }} />
      </span>
      {pct}%
    </span>
  );
}
