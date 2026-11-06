import { useState } from "react";
import type { RunDetail } from "@bindings/RunDetail";
import { CopyButton, Prose, time } from "../components";
import { FOLD_INPUT, foldText, KICKOFF_SEPARATOR } from "../lib/messageText";
import { noticeLines } from "../lib/notices";
import type { TranscriptTurn } from "../session";
import "./sessionmessage.css";

/** A person's message, or one recorded before senders were. */
export const fromPerson = (turn: TranscriptTurn) => !turn.from || turn.from.kind === "human";

/** What the message carried to the agent besides the person's words, kept out of the bubble's text. */
export function RideAlong({ notices }: { notices?: string[] }) {
  const lines = noticeLines(notices);
  if (!lines.length) return null;
  return (
    <details className="bubble-notices">
      <summary>随这条消息送达 {lines.length} 条提示</summary>
      <ul>
        {lines.map((line, i) => (
          <li key={i}>{line}</li>
        ))}
      </ul>
    </details>
  );
}

export function Bubble({ turn, tree }: { turn: TranscriptTurn; tree: RunDetail[] }) {
  const [open, setOpen] = useState(false);
  // Still the raw input: the fold below is what gets cut, never this.
  const request = tree.find(({ run }) => turn.input.startsWith(run.request.trim() + KICKOFF_SEPARATOR))?.run.request.trim();
  const visible = request === undefined || open ? turn.input : request;
  const long = request === undefined && turn.input.length > FOLD_INPUT;
  // Cut before parsing, so a fold in the middle of a fence or a link stays text (lib/messageText).
  const shown = long && !open ? foldText(turn.input) : visible;
  return (
    <div className="bubble bubble-md" title={time(turn.sent_at)}>
      <div className="bubble-text">
        <Prose text={shown} />
      </div>
      <RideAlong notices={turn.notices} />
      <div className="bubble-foot">
        {request !== undefined && (
          <button className="tools-more" aria-expanded={open} onClick={() => setOpen(!open)}>
            {open ? "收起 run 上下文 ▾" : "已附上 run 上下文（成员、校验、会话）▸"}
          </button>
        )}
        {long && (
          <button className="tools-more" onClick={() => setOpen(!open)}>
            {open ? "收起" : `展开全部（${turn.input.length} 字）`}
          </button>
        )}
        <CopyButton className="msg-copy" text={turn.input} title="复制这条消息的 markdown 原文" />
      </div>
    </div>
  );
}

/** One stretch of the agent's reply, finished or still streaming (`text` is the whole of it so far). */
export function AgentText({ text }: { text: string }) {
  return (
    <div className="agent-text">
      <Prose text={text} />
      <div className="copy-row">
        <CopyButton className="msg-copy" text={text} title="复制这段回复的 markdown 原文" />
      </div>
    </div>
  );
}
