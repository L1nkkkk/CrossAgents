import { useState } from "react";
import type { RunDetail } from "@bindings/RunDetail";
import { Prose, time } from "../components";
import type { TranscriptTurn } from "../session";

/** A person's message, or one recorded before senders were. */
export const fromPerson = (turn: TranscriptTurn) => !turn.from || turn.from.kind === "human";

/** Inputs longer than this start folded. */
const FOLD_INPUT = 600;

// Keep in sync with the kickoff separator in crates/orchestrator/src/brief.rs.
const KICKOFF_SEPARATOR = "\n\n---\nrun: ";

export function Bubble({ turn, tree }: { turn: TranscriptTurn; tree: RunDetail[] }) {
  const [open, setOpen] = useState(false);
  const request = tree.find(({ run }) => turn.input.startsWith(run.request.trim() + KICKOFF_SEPARATOR))?.run.request.trim();
  const visible = request === undefined || open ? turn.input : request;
  const long = request === undefined && turn.input.length > FOLD_INPUT;
  return (
    <div className="bubble" title={time(turn.sent_at)}>
      {long && !open ? visible.slice(0, FOLD_INPUT).trimEnd() + "…" : visible}
      {request !== undefined && (
        <div>
          <button className="tools-more" style={{ padding: 0, marginTop: 4 }} aria-expanded={open} onClick={() => setOpen(!open)}>
            {open ? "收起 run 上下文 ▾" : "已附上 run 上下文（成员、校验、会话）▸"}
          </button>
        </div>
      )}
      {long && (
        <div>
          <button className="tools-more" style={{ padding: 0, marginTop: 4 }} onClick={() => setOpen(!open)}>
            {open ? "收起" : `展开全部（${turn.input.length} 字）`}
          </button>
        </div>
      )}
    </div>
  );
}

/** One stretch of the agent's reply, finished or still streaming. */
export function AgentText({ text }: { text: string }) {
  return <Prose text={text} />;
}
