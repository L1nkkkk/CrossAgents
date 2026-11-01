// The avatar gallery in settings: upload, look, remove. The hub hands pictures out (hub/avatars.rs).

import { useRef, useState, type ChangeEvent } from "react";
import { api, errorText } from "../api";
import { avatarFromFile, useAvatarGallery, useAvatarImage } from "../avatarImages";
import { ErrorBox, Icon } from "../components";

const UPLOAD_BATCH = 50;

export function GalleryThumb({ id, size = 48 }: { id: string; size?: number }) {
  const url = useAvatarImage(id);
  return url ? <img className="gallery-img" src={url} width={size} height={size} alt="" draggable={false} /> : <span className="gallery-img" style={{ width: size, height: size }} />;
}

export function AvatarGallery() {
  const gallery = useAvatarGallery();
  const input = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [note, setNote] = useState<string>();
  const ids = gallery.data ?? [];

  async function upload(e: ChangeEvent<HTMLInputElement>) {
    const files = [...(e.target.files ?? [])];
    e.target.value = "";
    if (!files.length) return;
    setBusy(true);
    setError(undefined);
    setNote(undefined);
    try {
      const images = await Promise.all(files.map(avatarFromFile));
      let added = 0;
      let assigned = 0;
      for (let i = 0; i < images.length; i += UPLOAD_BATCH) {
        const done = await api.uploadAvatars({ images: images.slice(i, i + UPLOAD_BATCH) });
        added += done.added.length;
        assigned += done.assigned;
      }
      setNote(assigned ? `加了 ${added} 张，给 ${assigned} 个会话分配了头像。` : `加了 ${added} 张。`);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
      gallery.reload();
    }
  }

  async function remove(id: string) {
    if (!confirm("从图库删除这张图？正在用它的会话会退回字母头像。")) return;
    setError(undefined);
    setNote(undefined);
    try {
      await api.deleteAvatar(id);
    } catch (err) {
      setError(errorText(err));
    } finally {
      gallery.reload();
    }
  }

  return (
    <div className="setting-card">
      <div className="setting-line">
        <div>
          <div className="setting-label">头像图库</div>
          <div className="setting-hint">没有自己头像的会话会从这里分到一张，优先用得最少的，分到后不再变。emoji 或文字头像不受影响。</div>
        </div>
        <button type="button" disabled={busy} onClick={() => input.current?.click()}>
          <Icon name="plus" size={14} />
          {busy ? "上传中…" : "上传图片"}
        </button>
        <input ref={input} type="file" accept="image/png,image/jpeg,image/webp" multiple hidden onChange={upload} />
      </div>
      <ErrorBox error={error ?? gallery.error} />
      {note && <p className="setting-hint gallery-note">{note}</p>}
      {ids.length > 0 && (
        <div className="gallery-grid">
          {ids.map((id) => (
            <span className="gallery-item" key={id}>
              <GalleryThumb id={id} />
              <button type="button" className="icon-btn gallery-remove" onClick={() => void remove(id)} title="从图库删除" aria-label="从图库删除">
                <Icon name="close" size={11} />
              </button>
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

/** The editor's choice of picture: one from the gallery, another at random, or back to text. */
export function AvatarChoice({ current, onChoose, onText }: { current: string | null; onChoose: (id: string) => void; onText: () => void }) {
  const gallery = useAvatarGallery();
  const ids = gallery.data ?? [];
  if (!ids.length) return null;
  const others = ids.filter((id) => id !== current);
  return (
    <div className="avatar-choice">
      <div className="avatar-choice-actions">
        <button type="button" disabled={!others.length} onClick={() => onChoose(others[Math.floor(Math.random() * others.length)])}>
          随机换一张
        </button>
        {current && (
          <button type="button" onClick={onText}>
            用文字
          </button>
        )}
      </div>
      <div className="gallery-grid compact" role="radiogroup" aria-label="从图库选头像">
        {ids.map((id) => (
          <button type="button" key={id} role="radio" aria-checked={id === current} className={`gallery-pick ${id === current ? "on" : ""}`} onClick={() => onChoose(id)}>
            <GalleryThumb id={id} size={32} />
          </button>
        ))}
      </div>
    </div>
  );
}
