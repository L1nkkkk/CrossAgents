// Multi-Model Council launcher & live side-by-side comparison modal.
// Consults 2..=5 models across vendors in parallel (Independent 1-round or Debate 2-round),
// persists results in SQLite via `/v1/council`, and lets the user send a structured digest
// straight back to the current session's composer.

import { useEffect, useMemo, useState } from "react";
import { Check, History, Loader2, Plus, Scale, Send, Trash2, X } from "lucide-react";
import type { Agent } from "@bindings/Agent";
import type { CouncilMemberSpec } from "@bindings/CouncilMemberSpec";
import type { CouncilMode } from "@bindings/CouncilMode";
import type { CouncilRecord } from "@bindings/CouncilRecord";
import type { PluginInfo } from "@bindings/PluginInfo";
import { api, errorText } from "./api";
import { ErrorBox, ModelMark, Prose, ago, vendorName } from "./components";
import { prettyModel } from "./lib/models";

interface SeatDraft {
  key: number;
  plugin: string;
  model: string;
  perspective: string;
}

let seatSeq = 0;

function formatDigest(record: CouncilRecord): string {
  const lines: string[] = [
    `以下是多模型 Council（${record.mode === "debate" ? "2 轮交叉互评" : "独立并行"}）针对问题「${record.question}」的各席位意见，请你综合分析并给出最终方案：`,
    "",
  ];
  for (const m of record.members) {
    lines.push(`### Seat ${m.seat} · ${m.label}${m.model ? ` (${m.model})` : ""}`);
    if (m.status === "error" && !m.round1) {
      lines.push(`（失败：${m.error || "未知错误"}）`);
    } else {
      if (m.round1) lines.push(m.round1);
      if (m.round2) {
        lines.push("", `**Round 2 交叉互评结论：**`, m.round2);
      }
    }
    lines.push("");
  }
  return lines.join("\n").trim();
}

export function CouncilModal({
  agent,
  initialQuestion = "",
  onClose,
  onSendDigest,
}: {
  agent: Agent;
  initialQuestion?: string;
  onClose: () => void;
  onSendDigest?: (markdown: string) => void;
}) {
  const [tab, setTab] = useState<"new" | "view">("new");
  const [plugins, setPlugins] = useState<PluginInfo[]>([]);
  const [history, setHistory] = useState<CouncilRecord[]>([]);
  const [active, setActive] = useState<CouncilRecord | null>(null);
  const [question, setQuestion] = useState(initialQuestion);
  const [context, setContext] = useState("");
  const [mode, setMode] = useState<CouncilMode>("independent");
  const [seats, setSeats] = useState<SeatDraft[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  // Load installed plugins & recent council history on open.
  useEffect(() => {
    let live = true;
    void api.listPlugins(agent.node ?? undefined).then(
      (res) => {
        if (!live) return;
        const usable = res.plugins.filter((p) => p.models.length > 0 && (!p.cli || p.cli.installed));
        setPlugins(usable);
        setSeats((prev) => {
          if (prev.length >= 2) return prev;
          const picks: SeatDraft[] = [];
          // Default: pick up to 2 distinct vendors/models so cross-model value is immediate.
          for (const p of usable) {
            const m = p.models[0];
            if (m) picks.push({ key: ++seatSeq, plugin: p.name, model: m, perspective: "" });
            if (picks.length >= 2) break;
          }
          if (picks.length === 1 && usable[0]?.models[1]) {
            picks.push({ key: ++seatSeq, plugin: usable[0].name, model: usable[0].models[1], perspective: "" });
          }
          if (picks.length === 0) {
            picks.push(
              { key: ++seatSeq, plugin: agent.plugin, model: agent.model ?? "", perspective: "" },
              { key: ++seatSeq, plugin: agent.plugin, model: "", perspective: "" },
            );
          }
          return picks;
        });
      },
      (err) => {
        if (live) setError(errorText(err));
      },
    );
    void api.listCouncils(20).then(
      (res) => {
        if (live) setHistory(res.councils);
      },
      () => {},
    );
    return () => {
      live = false;
    };
  }, [agent.model, agent.node, agent.plugin]);

  // Poll live progress while the active council is still running.
  useEffect(() => {
    if (!active || active.status !== "running") return;
    let live = true;
    const timer = window.setInterval(() => {
      void api.getCouncil(active.id).then(
        (fresh) => {
          if (!live) return;
          setActive(fresh);
          setHistory((prev) => {
            const rest = prev.filter((x) => x.id !== fresh.id);
            return [fresh, ...rest];
          });
        },
        () => {},
      );
    }, 1200);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, [active?.id, active?.status]);

  const modelChoices = useMemo(() => {
    const out: { value: string; plugin: string; model: string; label: string }[] = [];
    for (const p of plugins) {
      for (const m of p.models) {
        out.push({
          value: `${p.name}::${m}`,
          plugin: p.name,
          model: m,
          label: `${vendorName(p.name)} · ${prettyModel(m)}`,
        });
      }
    }
    return out;
  }, [plugins]);

  const addSeat = () => {
    if (seats.length >= 5) return;
    const used = new Set(seats.map((s) => `${s.plugin}::${s.model}`));
    const next = modelChoices.find((c) => !used.has(c.value)) ?? modelChoices[0];
    setSeats((prev) => [
      ...prev,
      {
        key: ++seatSeq,
        plugin: next?.plugin ?? agent.plugin,
        model: next?.model ?? "",
        perspective: "",
      },
    ]);
  };

  async function launch() {
    if (!question.trim()) {
      setError("请先填写要咨询的问题");
      return;
    }
    const members: CouncilMemberSpec[] = seats
      .filter((s) => s.plugin || s.model)
      .map((s) => ({
        label: s.model ? `${vendorName(s.plugin)} · ${prettyModel(s.model)}` : vendorName(s.plugin),
        plugin: s.plugin || undefined,
        model: s.model || undefined,
        perspective: s.perspective.trim() || undefined,
      }));
    if (members.length < 2 || members.length > 5) {
      setError("Council 需要 2 到 5 个模型席位");
      return;
    }
    setBusy(true);
    setError(undefined);
    try {
      const started = await api.startCouncil(
        {
          question: question.trim(),
          context: context.trim() || undefined,
          mode,
          members,
          cwd: agent.cwd,
          requested_by: agent.id,
        },
        false,
      );
      setActive(started);
      setHistory((prev) => [started, ...prev.filter((x) => x.id !== started.id)]);
      setTab("view");
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="media-modal council-backdrop" role="dialog" aria-label="多模型 Council" onClick={onClose}>
      <div className="council-sheet" onClick={(e) => e.stopPropagation()}>
        <div className="council-bar">
          <div className="row" style={{ gap: 8 }}>
            <Scale size={16} />
            <strong>多模型 Council</strong>
            <div className="council-tabs">
              <button
                type="button"
                className={tab === "new" ? "active" : ""}
                onClick={() => setTab("new")}
              >
                新建会诊
              </button>
              <button
                type="button"
                className={tab === "view" ? "active" : ""}
                onClick={() => {
                  setTab("view");
                  if (!active && history[0]) setActive(history[0]);
                }}
              >
                <History size={13} />
                <span>结果与历史 ({history.length})</span>
              </button>
            </div>
          </div>
          <button type="button" className="ghost icon" onClick={onClose} aria-label="关闭">
            <X size={16} />
          </button>
        </div>

        {error && (
          <div style={{ padding: "8px 16px 0" }}>
            <ErrorBox error={error} />
          </div>
        )}

        {tab === "new" ? (
          <div className="council-form">
            <label className="field">
              <span>讨论问题 / 决策议题</span>
              <textarea
                rows={3}
                value={question}
                onChange={(e) => setQuestion(e.target.value)}
                placeholder="例如：移动端跨网络播放大体积音视频，是用分段 base64 JSON 还是 HTTP Range 流式传输更稳？"
              />
            </label>

            <label className="field">
              <span>背景上下文（可选：约束、代码片段或前提条件）</span>
              <textarea
                rows={2}
                value={context}
                onChange={(e) => setContext(e.target.value)}
                placeholder="可选，共享给所有模型席位参考…"
              />
            </label>

            <div className="council-mode-row">
              <span className="muted small">讨论模式</span>
              <div className="council-seg">
                <button
                  type="button"
                  className={mode === "independent" ? "active" : ""}
                  onClick={() => setMode("independent")}
                >
                  独立并行（1 轮）
                </button>
                <button
                  type="button"
                  className={mode === "debate" ? "active" : ""}
                  onClick={() => setMode("debate")}
                >
                  交叉互评（2 轮 Debate）
                </button>
              </div>
              <span className="muted tiny">
                {mode === "debate"
                  ? "Round 1 独立作答，Round 2 匿名互看其他席位结论并指出盲区"
                  : "所有席位互不可见，独立并行给出观点与建议"}
              </span>
            </div>

            <div className="council-seats-head">
              <strong>参会席位（{seats.length}/5）</strong>
              {seats.length < 5 && (
                <button type="button" className="ghost small" onClick={addSeat}>
                  <Plus size={13} />
                  <span>加一个席位</span>
                </button>
              )}
            </div>

            <div className="council-seats-list">
              {seats.map((seat, i) => (
                <div key={seat.key} className="council-seat-row">
                  <span className="council-seat-badge">{String.fromCharCode(65 + i)}</span>
                  <select
                    value={`${seat.plugin}::${seat.model}`}
                    onChange={(e) => {
                      const [plugin, model] = e.target.value.split("::");
                      setSeats((prev) =>
                        prev.map((x) => (x.key === seat.key ? { ...x, plugin: plugin ?? "", model: model ?? "" } : x)),
                      );
                    }}
                  >
                    {!modelChoices.some((c) => c.value === `${seat.plugin}::${seat.model}`) && (
                      <option value={`${seat.plugin}::${seat.model}`}>
                        {vendorName(seat.plugin)} · {seat.model || "默认模型"}
                      </option>
                    )}
                    {modelChoices.map((c) => (
                      <option key={c.value} value={c.value}>
                        {c.label}
                      </option>
                    ))}
                  </select>
                  <input
                    className="council-persp"
                    value={seat.perspective}
                    onChange={(e) =>
                      setSeats((prev) =>
                        prev.map((x) => (x.key === seat.key ? { ...x, perspective: e.target.value } : x)),
                      )
                    }
                    placeholder="视角（可选，如：安全/性能/成本）"
                  />
                  {seats.length > 2 && (
                    <button
                      type="button"
                      className="ghost icon"
                      onClick={() => setSeats((prev) => prev.filter((x) => x.key !== seat.key))}
                      aria-label="移除席位"
                    >
                      <Trash2 size={13} />
                    </button>
                  )}
                </div>
              ))}
            </div>

            <div className="council-foot">
              <span className="muted tiny">
                临时会话结束后会自动归档，不会弄乱侧栏；若某个模型不可用会如实报错，绝不静默替换。
              </span>
              <button type="button" className="primary" disabled={busy || !question.trim()} onClick={() => void launch()}>
                {busy ? <Loader2 size={14} className="spin" /> : <Scale size={14} />}
                <span>{busy ? "启动中…" : "发起 Council"}</span>
              </button>
            </div>
          </div>
        ) : (
          <div className="council-results-wrap">
            {history.length > 0 && (
              <div className="council-history-bar">
                {history.slice(0, 10).map((item) => (
                  <button
                    key={item.id}
                    type="button"
                    className={`council-hist-pill ${active?.id === item.id ? "active" : ""}`}
                    onClick={() => setActive(item)}
                    title={item.question}
                  >
                    <span className={`dot ${item.status === "running" ? "accent pulse" : item.status === "completed" ? "ok" : "danger"}`} />
                    <span className="council-hist-text">{item.question}</span>
                    <span className="muted tiny">{ago(item.created_at)}</span>
                  </button>
                ))}
              </div>
            )}

            {!active ? (
              <div className="council-empty">
                <p className="muted">还没有 Council 会诊记录，点击上方「新建会诊」即可发起多模型对比。</p>
              </div>
            ) : (
              <div className="council-detail">
                <div className="council-detail-head">
                  <div style={{ minWidth: 0, flex: 1 }}>
                    <div className="council-detail-q">{active.question}</div>
                    <div className="muted tiny">
                      {active.mode === "debate" ? "2 轮交叉互评" : "独立并行"} · {active.members.length} 个席位 ·{" "}
                      {active.status === "running" ? "进行中…" : active.status === "completed" ? "已完成" : "失败"}
                    </div>
                  </div>
                  {onSendDigest && active.status !== "running" && (
                    <button
                      type="button"
                      className="small"
                      onClick={() => {
                        onSendDigest(formatDigest(active));
                        onClose();
                      }}
                    >
                      <Send size={13} />
                      <span>汇总填入当前会话</span>
                    </button>
                  )}
                </div>

                <div className="council-grid">
                  {active.members.map((m) => (
                    <article key={m.seat} className={`council-card ${m.status}`}>
                      <header className="council-card-head">
                        <span className="council-seat-badge">{m.seat}</span>
                        <ModelMark plugin={m.plugin} model={m.model} size={14} />
                        <span className="council-card-label" title={m.model ?? m.plugin}>
                          {m.label}
                        </span>
                        <span className="spacer" />
                        {m.status === "running" && (
                          <span className="chip accent tiny">
                            <Loader2 size={11} className="spin" />
                            思考中
                          </span>
                        )}
                        {m.status === "ok" && (
                          <span className="chip ok tiny">
                            <Check size={11} />
                            {m.duration_ms ? `${(m.duration_ms / 1000).toFixed(1)}s` : "完成"}
                          </span>
                        )}
                        {m.status === "error" && <span className="chip danger tiny">失败</span>}
                      </header>
                      <div className="council-card-body">
                        {m.error && <div className="media-error" style={{ marginBottom: 8 }}>{m.error}</div>}
                        {m.round1 ? (
                          <Prose text={m.round1} />
                        ) : m.status === "running" ? (
                          <p className="muted small">等待模型独立作答…</p>
                        ) : null}
                        {m.round2 && (
                          <div className="council-r2">
                            <div className="council-r2-tag">Round 2 · 交叉互评与修正</div>
                            <Prose text={m.round2} />
                          </div>
                        )}
                      </div>
                    </article>
                  ))}
                </div>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
