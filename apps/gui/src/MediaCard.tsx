// Rich inline media & document cards for both agent replies (`![alt](path)` / `[label](audio.mp3)`)
// and user-sent attachments. Fetches bytes in chunks through the hub API and plays/previews from
// local Blob ObjectURLs so local desktop, remote desktop (SSH / iroh) and web all work identically.

import { useCallback, useEffect, useState } from "react";
import { Download, Eye, FileAudio, FileText, FileVideo, Film, File as FileIcon, Loader2, Play, X } from "lucide-react";
import { CopyButton } from "./components";
import { bytes, type Kind } from "./lib/attachments";
import {
  attachmentCacheKey,
  getCachedMedia,
  loadAgentMedia,
  loadAttachmentBlob,
  mediaCacheKey,
  mediaErrorText,
  triggerDownload,
  type LoadedMedia,
} from "./lib/mediaLoader";

type MediaSource =
  | { kind: "agentMedia"; agent: string; path: string }
  | { kind: "attachment"; agent: string; id: string; mime: string };

const AUTO_LOAD_AUDIO_BYTES = 8 * 1024 * 1024;
const AUTO_LOAD_VIDEO_BYTES = 3 * 1024 * 1024;

function extBadge(name: string, fallback: string): string {
  const clean = name.split(/[\\/]/).pop() ?? name;
  const dot = clean.lastIndexOf(".");
  if (dot > 0 && dot < clean.length - 1) return clean.slice(dot + 1).slice(0, 5).toUpperCase();
  return fallback.toUpperCase();
}

function fileNameOf(pathOrName: string): string {
  const clean = pathOrName.split(/[?#]/)[0] ?? pathOrName;
  return clean.split(/[\\/]/).pop() || clean;
}

export function MediaCard({
  source,
  mediaKind,
  label,
  sizeHint,
}: {
  source: MediaSource;
  mediaKind: Exclude<Kind, "image">;
  label: string;
  sizeHint?: number;
}) {
  const cacheKey =
    source.kind === "agentMedia"
      ? mediaCacheKey(source.agent, source.path)
      : attachmentCacheKey(source.agent, source.id);

  const [loaded, setLoaded] = useState<LoadedMedia | undefined>(() => getCachedMedia(cacheKey));
  const [loading, setLoading] = useState(false);
  const [progress, setProgress] = useState<{ loaded: number; total: number } | undefined>();
  const [error, setError] = useState<string | undefined>();
  const [pdfOpen, setPdfOpen] = useState(false);
  const [textOpen, setTextOpen] = useState(false);

  const displayName = loaded?.name || fileNameOf(source.kind === "agentMedia" ? source.path : label);
  const titleText = label && label !== displayName ? label : displayName;
  const shownSize = loaded?.size ?? sizeHint;

  const fetchMedia = useCallback(async (): Promise<LoadedMedia | undefined> => {
    const cached = getCachedMedia(cacheKey);
    if (cached) {
      setLoaded(cached);
      return cached;
    }
    setLoading(true);
    setError(undefined);
    setProgress(undefined);
    try {
      const onProgress = (got: number, total: number) => setProgress({ loaded: got, total });
      const res =
        source.kind === "agentMedia"
          ? await loadAgentMedia(source.agent, source.path, onProgress)
          : await loadAttachmentBlob(source.agent, source.id, label, source.mime, onProgress);
      setLoaded(res);
      return res;
    } catch (err) {
      setError(mediaErrorText(err));
      return undefined;
    } finally {
      setLoading(false);
    }
  }, [cacheKey, label, source]);

  // Auto-load audio (so TTS replies are immediately ready to play!) and small videos.
  useEffect(() => {
    if (loaded || loading || error) return;
    const shouldAuto =
      (mediaKind === "audio" && (sizeHint === undefined || sizeHint <= AUTO_LOAD_AUDIO_BYTES)) ||
      (mediaKind === "video" && sizeHint !== undefined && sizeHint <= AUTO_LOAD_VIDEO_BYTES);
    if (!shouldAuto) return;
    let live = true;
    void fetchMedia().then((res) => {
      if (!live) return;
      if (res) setLoaded(res);
    });
    return () => {
      live = false;
    };
  }, [cacheKey, error, fetchMedia, loaded, loading, mediaKind, sizeHint]);

  const handleDownload = async () => {
    const res = loaded ?? (await fetchMedia());
    if (res) triggerDownload(res.url, res.name || displayName);
  };

  const handlePreviewPdf = async () => {
    const res = loaded ?? (await fetchMedia());
    if (res) setPdfOpen(true);
  };

  const handleToggleText = async () => {
    if (textOpen) {
      setTextOpen(false);
      return;
    }
    const res = loaded ?? (await fetchMedia());
    if (res) setTextOpen(true);
  };

  const pct =
    progress && progress.total > 0 ? Math.min(100, Math.round((progress.loaded / progress.total) * 100)) : undefined;

  if (mediaKind === "audio") {
    return (
      <div className="media-card media-audio" title={source.kind === "agentMedia" ? source.path : displayName}>
        <div className="media-head">
          <span className="media-icon audio">
            <FileAudio size={15} />
          </span>
          <div className="media-meta">
            <span className="media-title">{titleText}</span>
            <span className="media-sub">
              {extBadge(displayName, "AUDIO")}
              {shownSize !== undefined ? ` · ${bytes(shownSize)}` : ""}
            </span>
          </div>
          <div className="media-actions">
            {!loaded && !loading && (
              <button type="button" className="ghost small" onClick={() => void fetchMedia()}>
                <Play size={13} />
                <span>加载</span>
              </button>
            )}
            <button type="button" className="ghost icon" onClick={() => void handleDownload()} title="下载音频" aria-label="下载音频">
              <Download size={14} />
            </button>
          </div>
        </div>
        {loaded && (
          <audio className="media-audio-el" controls preload="metadata" src={loaded.url}>
            您的环境不支持音频播放
          </audio>
        )}
        {loading && (
          <div className="media-status">
            <Loader2 size={13} className="spin" />
            <span>加载音频中{pct !== undefined ? ` ${pct}%` : "…"}</span>
            {pct !== undefined && (
              <span className="media-bar">
                <span style={{ width: `${pct}%` }} />
              </span>
            )}
          </div>
        )}
        {error && <div className="media-error">{error}</div>}
      </div>
    );
  }

  if (mediaKind === "video") {
    return (
      <div className="media-card media-video" title={source.kind === "agentMedia" ? source.path : displayName}>
        <div className="media-head">
          <span className="media-icon video">
            <FileVideo size={15} />
          </span>
          <div className="media-meta">
            <span className="media-title">{titleText}</span>
            <span className="media-sub">
              {extBadge(displayName, "VIDEO")}
              {shownSize !== undefined ? ` · ${bytes(shownSize)}` : ""}
            </span>
          </div>
          <div className="media-actions">
            {!loaded && !loading && (
              <button type="button" className="ghost small" onClick={() => void fetchMedia()}>
                <Film size={13} />
                <span>加载播放</span>
              </button>
            )}
            <button type="button" className="ghost icon" onClick={() => void handleDownload()} title="下载视频" aria-label="下载视频">
              <Download size={14} />
            </button>
          </div>
        </div>
        {loaded && (
          <video className="media-video-el" controls playsInline preload="metadata" src={loaded.url}>
            您的环境不支持视频播放
          </video>
        )}
        {loading && (
          <div className="media-status">
            <Loader2 size={13} className="spin" />
            <span>加载视频中{pct !== undefined ? ` ${pct}%` : "…"}</span>
            {pct !== undefined && (
              <span className="media-bar">
                <span style={{ width: `${pct}%` }} />
              </span>
            )}
          </div>
        )}
        {error && <div className="media-error">{error}</div>}
      </div>
    );
  }

  if (mediaKind === "pdf") {
    return (
      <>
        <div className="media-card media-doc" title={source.kind === "agentMedia" ? source.path : displayName}>
          <div className="media-head">
            <span className="media-icon pdf">PDF</span>
            <div className="media-meta">
              <span className="media-title">{titleText}</span>
              <span className="media-sub">
                PDF{shownSize !== undefined ? ` · ${bytes(shownSize)}` : ""}
              </span>
            </div>
            <div className="media-actions">
              <button type="button" className="ghost small" onClick={() => void handlePreviewPdf()} disabled={loading}>
                {loading ? <Loader2 size={13} className="spin" /> : <Eye size={13} />}
                <span>预览</span>
              </button>
              <button type="button" className="ghost icon" onClick={() => void handleDownload()} title="下载 PDF" aria-label="下载 PDF">
                <Download size={14} />
              </button>
            </div>
          </div>
          {loading && pct !== undefined && (
            <div className="media-status">
              <span>加载中 {pct}%</span>
              <span className="media-bar">
                <span style={{ width: `${pct}%` }} />
              </span>
            </div>
          )}
          {error && <div className="media-error">{error}</div>}
        </div>
        {pdfOpen && loaded && (
          <div className="media-modal" role="dialog" aria-label={displayName} onClick={() => setPdfOpen(false)}>
            <div className="media-modal-sheet" onClick={(e) => e.stopPropagation()}>
              <div className="media-modal-bar">
                <span className="media-modal-title">{displayName}</span>
                <div className="media-actions">
                  <button type="button" className="ghost small" onClick={() => triggerDownload(loaded.url, loaded.name || displayName)}>
                    <Download size={13} />
                    <span>下载</span>
                  </button>
                  <button type="button" className="ghost icon" onClick={() => setPdfOpen(false)} aria-label="关闭预览">
                    <X size={16} />
                  </button>
                </div>
              </div>
              <iframe className="media-pdf-frame" src={loaded.url} title={displayName} />
            </div>
          </div>
        )}
      </>
    );
  }

  if (mediaKind === "text") {
    return (
      <div className="media-card media-text" title={source.kind === "agentMedia" ? source.path : displayName}>
        <div className="media-head">
          <span className="media-icon text">
            <FileText size={15} />
          </span>
          <div className="media-meta">
            <span className="media-title">{titleText}</span>
            <span className="media-sub">
              {extBadge(displayName, "TXT")}
              {shownSize !== undefined ? ` · ${bytes(shownSize)}` : ""}
            </span>
          </div>
          <div className="media-actions">
            <button type="button" className="ghost small" onClick={() => void handleToggleText()} disabled={loading}>
              {loading ? <Loader2 size={13} className="spin" /> : <Eye size={13} />}
              <span>{textOpen ? "收起" : "预览"}</span>
            </button>
            <button type="button" className="ghost icon" onClick={() => void handleDownload()} title="下载文件" aria-label="下载文件">
              <Download size={14} />
            </button>
          </div>
        </div>
        {textOpen && loaded && (
          <div className="media-text-body">
            {loaded.text !== undefined ? (
              <>
                <div className="media-text-tools">
                  <CopyButton text={loaded.text} label="复制内容" title="复制文件内容" className="ghost small" />
                </div>
                <pre className="media-text-pre">{loaded.text}</pre>
              </>
            ) : (
              <div className="media-status">文件较大或非 UTF-8 文本，请点击下载查看</div>
            )}
          </div>
        )}
        {error && <div className="media-error">{error}</div>}
      </div>
    );
  }

  // Generic binary/document file (zip, docx, xlsx, pptx, epub, ...)
  return (
    <div className="media-card media-file" title={source.kind === "agentMedia" ? source.path : displayName}>
      <div className="media-head">
        <span className="media-icon file">
          <FileIcon size={15} />
        </span>
        <div className="media-meta">
          <span className="media-title">{titleText}</span>
          <span className="media-sub">
            {extBadge(displayName, "FILE")}
            {shownSize !== undefined ? ` · ${bytes(shownSize)}` : ""}
          </span>
        </div>
        <div className="media-actions">
          <button type="button" className="ghost small" onClick={() => void handleDownload()} disabled={loading}>
            {loading ? <Loader2 size={13} className="spin" /> : <Download size={13} />}
            <span>下载</span>
          </button>
        </div>
      </div>
      {loading && pct !== undefined && (
        <div className="media-status">
          <span>下载中 {pct}%</span>
          <span className="media-bar">
            <span style={{ width: `${pct}%` }} />
          </span>
        </div>
      )}
      {error && <div className="media-error">{error}</div>}
    </div>
  );
}
