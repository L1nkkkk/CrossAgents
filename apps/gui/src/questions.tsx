// What agents put in front of the person (docs/tasks/H1.md §5.4): questions to answer right
// here, notices to read, and answers that were defaults because nobody answered in time.

import { useEffect, useState, type FormEvent } from "react";
import type { Agent } from "@bindings/Agent";
import type { HumanItem } from "@bindings/HumanItem";
import { api, errorText } from "./api";
import { Avatar, ErrorBox, displayName, sessionHref } from "./components";
import { useHubData } from "./hub";
import { remaining } from "./lib/policy";

/** Defaults taken within this long stay visible, so you can still decide otherwise. */
const RECENT_MS = 24 * 3600 * 1000;
/** Cards shown before the rest go behind "还有 N 个". */
const FIT = 3;

export function useQuestions() {
  return useHubData(() => api.questions(true), [], (e) => e.type.startsWith("human."));
}

/** Open questions and notices, newest first, then recent defaults. */
export function pending(items: HumanItem[], now = Date.now()): HumanItem[] {
  const open = items.filter((i) => i.status === "open");
  const defaulted = items.filter((i) => i.status === "defaulted" && now - new Date(i.answered_at ?? i.created_at).getTime() < RECENT_MS);
  return [...open, ...defaulted];
}

export function QuestionCards({ agents }: { agents: Agent[] }) {
  const questions = useQuestions();
  const [all, setAll] = useState(false);
  const items = pending(questions.data?.items ?? []);
  if (!items.length) return null;
  const shown = all ? items : items.slice(0, FIT);
  return (
    <div className="asks" role="region" aria-label="等你回答">
      {shown.map((item) => (
        <AskCard key={item.id} item={item} agent={agents.find((a) => a.id === item.from)} />
      ))}
      {items.length > shown.length && (
        <button className="tools-more" onClick={() => setAll(true)}>
          还有 {items.length - shown.length} 个
        </button>
      )}
    </div>
  );
}

function AskCard({ item, agent }: { item: HumanItem; agent?: Agent }) {
  const [text, setText] = useState("");
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [changing, setChanging] = useState(false);
  const [, tick] = useState(0);
  useEffect(() => {
    if (!item.timeout_at || item.status !== "open") return;
    const timer = setInterval(() => tick((n) => n + 1), 30_000);
    return () => clearInterval(timer);
  }, [item.timeout_at, item.status]);

  async function run(action: () => Promise<unknown>) {
    setBusy(true);
    setError(undefined);
    try {
      await action();
      setText("");
      setChanging(false);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }
  const answer = (value: string) => run(() => api.answer(item.id, value));
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (text.trim()) void answer(text.trim());
  };
  const who = agent ? (
    <a className="ask-who" href={sessionHref(agent)}>
      <Avatar agent={agent} size={18} />
      {displayName(agent)}
    </a>
  ) : (
    <span className="ask-who">agent</span>
  );

  if (item.kind === "notice") {
    return (
      <div className="ask ask-notice">
        <div className="ask-head">
          {who}
          <span className="muted small">告诉你</span>
          <div className="spacer" />
          <button className="ghost small" disabled={busy} onClick={() => run(() => api.dismiss(item.id))}>
            知道了
          </button>
        </div>
        <div className="ask-text">{item.text}</div>
        <ErrorBox error={error} />
      </div>
    );
  }

  if (item.status === "defaulted" && !changing) {
    return (
      <div className="ask defaulted">
        <div className="ask-head">
          {who}
          <span className="chip warn tiny">没等到回答，已按默认「{item.answer}」</span>
          <div className="spacer" />
          <button className="ghost small" onClick={() => setChanging(true)}>
            改答
          </button>
        </div>
        <div className="ask-text muted">{item.text}</div>
      </div>
    );
  }

  return (
    <form className={`ask `} onSubmit={submit}>
      <div className="ask-head">
        {who}
        <span className="muted small">{item.action === "budget" ? "预算用完了" : item.action === "approval" ? "等你批准" : "问你"}</span>
        <div className="spacer" />
        {item.timeout_at && item.status === "open" && (
          <span className="muted small" title={new Date(item.timeout_at).toLocaleString()}>
            {remaining(item.timeout_at)}按默认「{item.default}」
          </span>
        )}
      </div>
      {item.action === "approval" ? <ApprovalText text={item.text} /> : <div className="ask-text">{item.text}</div>}
      {item.options.length > 0 && (
        <div className="ask-options">
          {item.options.map((o) => (
            <button key={o} type="button" disabled={busy} className={o === item.default ? "default" : ""} onClick={() => answer(o)}>
              {o}
            </button>
          ))}
        </div>
      )}
      {!item.action && (
        <div className="ask-reply">
          <input value={text} onChange={(e) => setText(e.target.value)} placeholder={item.options.length ? "或者写你自己的回答…" : "你的回答…"} />
          <button type="submit" className="primary small" disabled={busy || !text.trim()}>
            回答
          </button>
        </div>
      )}
      <ErrorBox error={error} />
    </form>
  );
}

/** An approval (docs/tasks/DOGFOOD.md §7): who wants to do what and why, then the exact command. */
function ApprovalText({ text }: { text: string }) {
  const [what, ...rest] = text.split("\n");
  return (
    <div className="ask-text">
      {what}
      {rest.length > 0 && <pre className="approval-command">{rest.join("\n")}</pre>}
    </div>
  );
}
