// 需要你, in the event stream (docs/research/magpie-lancer.md §2.6, after Lancer): a question or
// an approval the person has to answer sits where it happened, with its options as buttons —
// the same item the strip above the composer shows, answered from either place.

import { useState, type FormEvent } from "react";
import type { Agent } from "@bindings/Agent";
import type { HumanItem } from "@bindings/HumanItem";
import { api, errorText } from "./api";
import { navigate } from "./App";
import { Avatar, ErrorBox, displayName, sessionHref } from "./components";
import { ApprovalText } from "./questions";
import { remaining } from "./lib/policy";
import "./inlineask.css";

export function InlineNeeds({ items, agents }: { items: HumanItem[]; agents: Agent[] }) {
  if (!items.length) return null;
  return (
    <div className="inline-needs" role="region" aria-label="需要你">
      <div className="in-head" title="和输入框上方的「需要你」是同一条待办：在这里答了，那边也就答了">
        需要你 · {items.length}
      </div>
      {items.map((item) => (
        <InlineAsk key={item.id} item={item} agent={agents.find((a) => a.id === item.from)} />
      ))}
    </div>
  );
}

/** What an item asks of the person, in one word. */
function actionLabel(item: HumanItem): string {
  if (item.kind === "notice") return "告诉你";
  switch (item.action) {
    case "approval":
      return "等你批准";
    case "budget":
      return "预算用完了";
    case "work":
      return "想在独立分支上动手";
    default:
      return "问你";
  }
}

function InlineAsk({ item, agent }: { item: HumanItem; agent?: Agent }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  async function answer(value: string) {
    setBusy(true);
    setError(undefined);
    try {
      const done = await api.answer(item.id, value);
      // A Work the person just started: go where it happens.
      if (done.action === "work" && done.run_id) navigate("runs", done.run_id);
      setText("");
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  async function dismiss() {
    setBusy(true);
    setError(undefined);
    try {
      await api.dismiss(item.id);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (text.trim()) void answer(text.trim());
  };

  return (
    <div className="ia" data-question={item.id}>
      <div className="ia-head">
        {agent ? (
          <a className="ia-who" href={sessionHref(agent)} title={`打开 ${displayName(agent)} 的会话`}>
            <Avatar agent={agent} size={16} />
            {displayName(agent)}
          </a>
        ) : (
          <span className="ia-who">agent</span>
        )}
        <span className="ia-what">{actionLabel(item)}</span>
        <span className="spacer" />
        {item.timeout_at && (
          <span className="ia-time" title={new Date(item.timeout_at).toLocaleString()}>
            {remaining(item.timeout_at)}按默认「{item.default}」
          </span>
        )}
      </div>
      <div className="ia-text">{item.action === "approval" ? <ApprovalText text={item.text} /> : item.text}</div>
      {item.detail && (
        <details className="ia-detail">
          <summary>交给 Work 的说明</summary>
          <div className="pre small">{item.detail}</div>
        </details>
      )}
      {item.kind === "notice" ? (
        <div className="ia-options">
          <button type="button" className="ia-opt" disabled={busy} onClick={() => void dismiss()}>
            知道了
          </button>
        </div>
      ) : (
        <>
          {item.options.length > 0 && (
            <div className="ia-options">
              {item.options.map((option) => (
                <button
                  key={option}
                  type="button"
                  className={`ia-opt ${option === item.default ? "default" : ""}`}
                  disabled={busy}
                  onClick={() => void answer(option)}
                  title={option === item.default ? "没人回答时按这个" : undefined}
                >
                  {option}
                </button>
              ))}
            </div>
          )}
          {!item.action && (
            <form className="ia-reply" onSubmit={submit}>
              <input
                value={text}
                onChange={(e) => setText(e.target.value)}
                disabled={busy}
                placeholder={item.options.length ? "或者写你自己的回答…" : "你的回答…"}
                aria-label="你的回答"
              />
              <button type="submit" className="primary small" disabled={busy || !text.trim()}>
                回答
              </button>
            </form>
          )}
        </>
      )}
      <ErrorBox error={error} />
    </div>
  );
}
