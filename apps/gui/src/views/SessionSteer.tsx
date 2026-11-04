import { createContext, useContext } from "react";
import type { Attachment } from "@bindings/Attachment";
import { TurnFiles } from "../attach";
import { CopyButton, Prose, useFileScope } from "../components";
import type { TranscriptItem } from "../session";
import "./steer.css";

type Steer = Extract<TranscriptItem, { type: "steer" }>;

/** The turn around this item has ended: a message still unread will not be read in it. */
export const TurnEnded = createContext(false);

/**
 * A message handed to the running turn (docs/research/cutex-gui-teardown.md §2.1). Until the
 * model reads it, it is a pale dashed bubble — inserted, not yet part of the conversation; once
 * read it is an ordinary message of yours, with its files. A turn that ended without reading it
 * says so, and the text can be copied to send again.
 */
export function SteerItem({ item }: { item: TranscriptItem }) {
  const ended = useContext(TurnEnded);
  if (item.type !== "steer") return null;
  return (
    <>
      {item.read ? (
        // Read: an ordinary message of yours, the same as Bubble's.
        <div className="bubble bubble-md">
          <div className="bubble-text">
            <Prose text={item.text} />
          </div>
          <div className="bubble-foot">
            <CopyButton className="msg-copy" text={item.text} title="复制这条消息的 markdown 原文" />
          </div>
        </div>
      ) : (
        <div className="steer-inserted">
          <div className="bubble bubble-md steer-waiting">
            <div className="bubble-text">
              <Prose text={item.text} />
            </div>
            {ended && (
              <div className="bubble-foot">
                <CopyButton className="msg-copy" text={item.text} title="复制这条消息的 markdown 原文" />
              </div>
            )}
          </div>
          <span className={ended ? "steer-note steer-lost" : "steer-note"}>{ended ? "模型没有读到这条插话，需要的话请重新发送" : "已插入，等待模型读取"}</span>
        </div>
      )}
      <SteerFiles item={item} />
    </>
  );
}

/**
 * The files it carried. Images are fetched per agent, and Session.tsx hands this item only the
 * item itself: a session's own page names its agent in the file scope, a run's or task's scope
 * does not, so there the files show as cards instead of thumbnails.
 */
function SteerFiles({ item }: { item: Steer }) {
  const scope = useFileScope();
  const files = item.attachments ?? [];
  if (!files.length) return null;
  const agent = scope?.kind === "agent" ? scope.agent : undefined;
  return agent ? <TurnFiles agent={agent} files={files} /> : <FileCards files={files} />;
}

function FileCards({ files }: { files: Attachment[] }) {
  return (
    <div className="turn-files-list steer-files">
      {files.map((f) => (
        <span key={f.id + f.name} className="file-chip" title={`${f.path} · ${f.mime}`}>
          <span className="file-name">{f.name}</span>
          <span className="muted tiny">{Math.max(1, Math.round(f.size / 1024))} KB</span>
        </span>
      ))}
    </div>
  );
}
