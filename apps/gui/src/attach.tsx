// Files with a message (docs/tasks/DOGFOOD.md §3): what the composer holds before sending
// (pasted screenshots, dropped or picked files), and the files a sent message carried.

import { useEffect, useRef, useState, type ClipboardEvent, type DragEvent } from "react";
import type { Attachment } from "@bindings/Attachment";
import type { AttachmentSupport } from "@bindings/AttachmentSupport";
import type { AttachmentUpload } from "@bindings/AttachmentUpload";
import { api } from "./api";
import { Icon } from "./components";
import { acceptFor, bytes, isImage, mimeOf, refusal, supportNote } from "./lib/attachments";

/** A file in the composer: what goes to the hub, and a preview for images. */
export type Held = AttachmentUpload & { key: number; size: number; preview?: string };

let nextKey = 0;

function read(file: File): Promise<Held> {
  return new Promise((done, fail) => {
    const reader = new FileReader();
    reader.onerror = () => fail(reader.error ?? new Error(`cannot read ${file.name}`));
    reader.onload = () => {
      const url = String(reader.result);
      const mime = mimeOf(file.name, file.type);
      done({ key: ++nextKey, name: file.name || "粘贴的图片.png", mime, size: file.size, data: url.slice(url.indexOf(",") + 1), preview: isImage(mime) ? url : undefined });
    };
    reader.readAsDataURL(file);
  });
}

/** The composer's files: add (checked against the hub's limits and the agent's kinds), remove, clear. */
export function useHeld(onError: (why: string | undefined) => void, support?: AttachmentSupport | null, driver?: string | null) {
  const [held, setHeld] = useState<Held[]>([]);
  const input = useRef<HTMLInputElement>(null);
  const canAttach = !support || support.image || support.pdf || support.text;
  const note = supportNote(support, driver);

  async function add(files: File[]) {
    if (!files.length) return;
    const why = refusal(
      held,
      files.map((f) => ({ name: f.name, mime: mimeOf(f.name, f.type), size: f.size })),
      support,
      driver,
    );
    onError(why);
    if (why) return;
    try {
      const read_ = await Promise.all(files.map(read));
      setHeld((h) => [...h, ...read_]);
    } catch (err) {
      onError(err instanceof Error ? err.message : String(err));
    }
  }

  return {
    held,
    clear: () => setHeld([]),
    restore: (files: Held[]) => setHeld(files),
    remove: (key: number) => setHeld((h) => h.filter((f) => f.key !== key)),
    onPaste: (e: ClipboardEvent) => {
      const files = [...e.clipboardData.files];
      if (files.length) {
        e.preventDefault();
        void add(files);
      }
    },
    onDrop: (e: DragEvent) => {
      if (!e.dataTransfer.files.length) return;
      e.preventDefault();
      void add([...e.dataTransfer.files]);
    },
    onDragOver: (e: DragEvent) => {
      if (e.dataTransfer.types.includes("Files")) e.preventDefault();
    },
    /** The paperclip and its hidden file input. */
    picker: (
      <>
        <button
          className="ghost icon"
          onClick={() => input.current?.click()}
          disabled={!canAttach}
          title={canAttach ? (note ?? "附加图片或文件（也可以粘贴截图、拖进来）") : (note ?? "这个 agent 不收文件")}
          aria-label={canAttach ? "附加文件" : "附加文件（不可用）"}
        >
          <Icon name="attach" size={15} />
        </button>
        <input
          ref={input}
          type="file"
          multiple
          hidden
          accept={acceptFor(support)}
          onChange={(e) => {
            void add([...(e.target.files ?? [])]);
            e.target.value = "";
          }}
        />
      </>
    ),
  };
}

/** The files waiting in the composer, each with a remove button. */
export function HeldFiles({ files, onRemove }: { files: Held[]; onRemove: (key: number) => void }) {
  if (!files.length) return null;
  return (
    <div className="held-files">
      {files.map((f) => (
        <span key={f.key} className="file-chip" title={`${f.name} · ${f.mime} · ${bytes(f.size)}`}>
          {f.preview ? <img src={f.preview} alt="" /> : <span className="file-ext">{ext(f.name)}</span>}
          <span className="file-name">{f.name}</span>
          <button className="ghost icon" onClick={() => onRemove(f.key)} aria-label={`去掉 ${f.name}`}>
            <Icon name="close" size={12} />
          </button>
        </span>
      ))}
    </div>
  );
}

/** Files a sent message carried: images as thumbnails (fetched from the hub), others as cards. */
export function TurnFiles({ agent, files }: { agent: string; files: Attachment[] }) {
  if (!files.length) return null;
  return (
    <div className="turn-files-list">
      {files.map((f) => (isImage(f.mime) ? <Thumb key={f.id + f.name} agent={agent} file={f} /> : <FileCard key={f.id + f.name} file={f} />))}
    </div>
  );
}

function FileCard({ file }: { file: Attachment }) {
  return (
    <span className="file-chip" title={`${file.path}`}>
      <span className="file-ext">{ext(file.name)}</span>
      <span className="file-name">{file.name}</span>
      <span className="muted tiny">{bytes(file.size)}</span>
    </span>
  );
}

function Thumb({ agent, file }: { agent: string; file: Attachment }) {
  const [src, setSrc] = useState<string>();
  useEffect(() => {
    let live = true;
    api.attachment(agent, file.id).then(
      (c) => live && setSrc(`data:${c.mime};base64,${c.data}`),
      () => {},
    );
    return () => {
      live = false;
    };
  }, [agent, file.id]);
  if (!src) return <FileCard file={file} />;
  return <ImageThumb src={src} alt={file.name} title={`${file.name} ? ${bytes(file.size)}`} />;
}

/** Shared attachment thumbnail and lightbox. */
export function ImageThumb({ src, alt, title }: { src: string; alt: string; title?: string }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button className="thumb" onClick={() => setOpen(true)} title={title ?? alt}>
        <img src={src} alt={alt} />
      </button>
      {open && (
        <div className="lightbox" onClick={() => setOpen(false)} role="dialog" aria-label={alt}>
          <img src={src} alt={alt} />
        </div>
      )}
    </>
  );
}

function ext(name: string): string {
  const e = name.split(".").pop() ?? "";
  return e && e !== name ? e.slice(0, 4).toUpperCase() : "FILE";
}
