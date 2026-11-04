// A reminder over the composer after the gateway's content moderation refused a whole turn.
// It never blocks sending: the same context is still there, so the person decides whether to
// start a new chat, compact the history, or carry on anyway.

import { useEffect, useState } from "react";
import type { Agent } from "@bindings/Agent";
import type { TranscriptTurn } from "@bindings/TranscriptTurn";
import { Icon } from "../components";
import { useCompact } from "../contextusage";
import { homeOf, isChat, selectWorkspace, wsKey } from "../workspace";
import "./moderation.css";

/** Where 新开会话 goes: the same entry the sidebar's 新会话 uses, in this session's workspace. */
const newChatWorkspace = (agent: Agent) => (isChat(agent) ? "" : wsKey(homeOf(agent)));

/** A fresh turn shows the reminder again, even if the last one was dismissed. */
export function SessionModeration({ agent, turn }: { agent: Agent; turn: TranscriptTurn }) {
  const [dismissed, setDismissed] = useState(false);
  const compact = useCompact(agent);
  useEffect(() => setDismissed(false), [turn.message_id]);
  if (dismissed) return null;

  const carryOn = () => {
    setDismissed(true);
    // The composer is untouched and still enabled; put the cursor where the person sends.
    document.querySelector<HTMLTextAreaElement>(".dock .composer textarea")?.focus();
  };

  return (
    <div className="moderation-bar" role="alert">
      <div className="moderation-head">
        <Icon name="info" size={15} />
        <strong>上下文已被污染</strong>
        <span className="spacer" />
        <button className="ghost icon" onClick={carryOn} title="收起提示" aria-label="收起提示">
          <Icon name="close" size={13} />
        </button>
      </div>
      <p>
        这一轮被网关的内容审查拦下了：它扫的是整段上下文，不只是你最后那句话。触发的内容还在历史里，
        原样重试只会再烧一次 token。
      </p>
      <div className="moderation-actions">
        <a className="button small" href="#/" onClick={() => selectWorkspace(newChatWorkspace(agent))}>
          新开会话
        </a>
        <button
          className="small"
          disabled={compact.busy || !!compact.blocked}
          onClick={() => void compact.compact()}
          title={compact.blocked ?? "让 agent 把历史压缩成摘要，腾出上下文"}
        >
          {compact.busy ? "压缩中…" : "压缩上下文"}
        </button>
        <button className="ghost small" onClick={carryOn} title="只是提醒：收起后可以在下面照常发送">
          仍要发送
        </button>
        {compact.blocked && <span className="moderation-why">{compact.blocked}</span>}
      </div>
      {compact.error && (
        <p className="moderation-error" role="alert">
          {compact.error}
        </p>
      )}
    </div>
  );
}
