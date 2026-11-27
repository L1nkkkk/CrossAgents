// Mobile-friendly inline media & document cards (iOS Safari / Android Chrome / PWA ready).
// Audio auto-loads when small so TTS replies play on first tap; videos tap-to-load with chunk
// progress to respect mobile data; PDFs open natively in a new tab or download cleanly.

import { useCallback, useEffect, useState } from "react";
import { Check, Copy, Download, ExternalLink, Eye, File as FileIcon, FileAudio, FileText, FileVideo, Loader2, Play } from "lucide-react";
import {
  formatBytes,
  getCachedMobileMedia,
  loadMobileAgentMedia,
  loadMobileAttachment,
  mobileAttKey,
  mobileMediaKey,
  triggerMobileDownload,
  type LoadedMobileMedia,
  type MediaKind,
} from "../media";
import type { HubTransport } from "../transport";

type MobileMediaSource =
  | { kind: "agentMedia"; agentId: string; path: string }
  | { kind: "attachment"; agentId: string; attId: string; mime: string };

const AUTO_AUDIO_CAP = 3.5 * 1024 * 1024;

function extBadge(name: string, fallback: string): string {
  const clean = name.split(/[\\/]/).pop() || name;
  const dot = clean.lastIndexOf(".");
  if (dot > 0 && dot < clean.length - 1) return clean.slice(dot + 1).slice(0, 5).toUpperCase();
  return fallback.toUpperCase();
}

function baseName(p: string): string {
  const clean = p.split(/[?#]/)[0] || p;
  return clean.split(/[\\/]/).pop() || clean;
}

export function MobileMediaCard({
  source,
  mediaKind,
  label,
  sizeHint,
  transport,
  onZoomImage,
}: {
  source: MobileMediaSource;
  mediaKind: MediaKind;
  label: string;
  sizeHint?: number;
  transport: HubTransport | null;
  onZoomImage?: (url: string) => void;
}) {
  const key =
    source.kind === "agentMedia"
      ? mobileMediaKey(source.agentId, source.path)
      : mobileAttKey(source.agentId, source.attId);

  const [loaded, setLoaded] = useState<LoadedMobileMedia | undefined>(() => getCachedMobileMedia(key));
  const [loading, setLoading] = useState(false);
  const [progress, setProgress] = useState<{ loaded: number; total: number } | undefined>();
  const [error, setError] = useState<string>();
  const [textOpen, setTextOpen] = useState(false);
  const [copied, setCopied] = useState(false);

  const displayName = loaded?.name || baseName(source.kind === "agentMedia" ? source.path : label);
  const titleText = label && label !== displayName ? label : displayName;
  const shownSize = loaded?.size ?? sizeHint;

  const fetchMedia = useCallback(async (): Promise<LoadedMobileMedia | undefined> => {
    if (!transport) {
      setError("离线状态下无法加载远程文件");
      return undefined;
    }
    const hit = getCachedMobileMedia(key);
    if (hit) {
      setLoaded(hit);
      return hit;
    }
    setLoading(true);
    setError(undefined);
    setProgress(undefined);
    try {
      const onProgress = (got: number, total: number) => setProgress({ loaded: got, total });
      const res =
        source.kind === "agentMedia"
          ? await loadMobileAgentMedia(transport, source.agentId, source.path, onProgress)
          : await loadMobileAttachment(transport, source.agentId, source.attId, label, source.mime, onProgress);
      setLoaded(res);
      return res;
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      return undefined;
    } finally {
      setLoading(false);
    }
  }, [key, label, source, transport]);

  // Auto-load images and small audio (so TTS audio replies play immediately on mobile!).
  useEffect(() => {
    if (!transport || loaded || loading || error) return;
    const shouldAuto =
      mediaKind === "image" ||
      (mediaKind === "audio" && (sizeHint === undefined || sizeHint <= AUTO_AUDIO_CAP));
    if (!shouldAuto) return;
    let live = true;
    void fetchMedia().then((res) => {
      if (live && res) setLoaded(res);
    });
    return () => {
      live = false;
    };
  }, [error, fetchMedia, loaded, loading, mediaKind, sizeHint, transport]);

  const pct =
    progress && progress.total > 0 ? Math.min(100, Math.round((progress.loaded / progress.total) * 100)) : undefined;

  if (mediaKind === "image") {
    if (loaded) {
      return (
        <div className="m-media-img-wrap">
          <img
            src={loaded.url}
            alt={titleText}
            className="m-media-img"
            onClick={() => onZoomImage?.(loaded.url)}
          />
        </div>
      );
    }
    return (
      <div className="m-media-card">
        <div className="m-media-head">
          <span className="m-media-title">{titleText}</span>
          {!loading && (
            <button type="button" className="m-media-btn" onClick={() => void fetchMedia()}>
              加载图片
            </button>
          )}
        </div>
        {loading && <div className="m-media-status">正在加载图片{pct !== undefined ? ` ${pct}%` : "…"}</div>}
        {error && <div className="m-media-error">{error}</div>}
      </div>
    );
  }

  if (mediaKind === "audio") {
    return (
      <div className="m-media-card m-media-audio">
        <div className="m-media-head">
          <span className="m-media-badge audio">
            <FileAudio size={15} />
          </span>
          <div className="m-media-meta">
            <div className="m-media-title">{titleText}</div>
            <div className="m-media-sub">
              {extBadge(displayName, "AUDIO")}
              {shownSize !== undefined ? ` · ${formatBytes(shownSize)}` : ""}
            </div>
          </div>
          <div className="m-media-actions">
            {!loaded && !loading && (
              <button type="button" className="m-media-btn" onClick={() => void fetchMedia()}>
                <Play size={12} />
                <span>加载</span>
              </button>
            )}
            <button
              type="button"
              className="m-media-icon-btn"
              aria-label="下载音频"
              onClick={async () => {
                const res = loaded ?? (await fetchMedia());
                if (res) triggerMobileDownload(res.url, res.name || displayName);
              }}
            >
              <Download size={14} />
            </button>
          </div>
        </div>
        {loaded && (
          <audio className="m-media-audio-el" controls playsInline preload="metadata" src={loaded.url}>
            当前浏览器不支持音频播放
          </audio>
        )}
        {loading && (
          <div className="m-media-status">
            <Loader2 size={13} className="spin" />
            <span>加载音频{pct !== undefined ? ` ${pct}%` : "…"}</span>
            {pct !== undefined && (
              <span className="m-media-bar">
                <span style={{ width: `${pct}%` }} />
              </span>
            )}
          </div>
        )}
        {error && <div className="m-media-error">{error}</div>}
      </div>
    );
  }

  if (mediaKind === "video") {
    return (
      <div className="m-media-card m-media-video">
        <div className="m-media-head">
          <span className="m-media-badge video">
            <FileVideo size={15} />
          </span>
          <div className="m-media-meta">
            <div className="m-media-title">{titleText}</div>
            <div className="m-media-sub">
              {extBadge(displayName, "VIDEO")}
              {shownSize !== undefined ? ` · ${formatBytes(shownSize)}` : ""}
            </div>
          </div>
          <div className="m-media-actions">
            {!loaded && !loading && (
              <button type="button" className="m-media-btn" onClick={() => void fetchMedia()}>
                <Play size={12} />
                <span>加载视频</span>
              </button>
            )}
            <button
              type="button"
              className="m-media-icon-btn"
              aria-label="下载视频"
              onClick={async () => {
                const res = loaded ?? (await fetchMedia());
                if (res) triggerMobileDownload(res.url, res.name || displayName);
              }}
            >
              <Download size={14} />
            </button>
          </div>
        </div>
        {loaded && (
          <video className="m-media-video-el" controls playsInline preload="metadata" src={loaded.url}>
            当前浏览器不支持视频播放
          </video>
        )}
        {loading && (
          <div className="m-media-status">
            <Loader2 size={13} className="spin" />
            <span>加载视频{pct !== undefined ? ` ${pct}%` : "…"}</span>
            {pct !== undefined && (
              <span className="m-media-bar">
                <span style={{ width: `${pct}%` }} />
              </span>
            )}
          </div>
        )}
        {error && <div className="m-media-error">{error}</div>}
      </div>
    );
  }

  if (mediaKind === "pdf") {
    return (
      <div className="m-media-card">
        <div className="m-media-head">
          <span className="m-media-badge pdf">PDF</span>
          <div className="m-media-meta">
            <div className="m-media-title">{titleText}</div>
            <div className="m-media-sub">PDF{shownSize !== undefined ? ` · ${formatBytes(shownSize)}` : ""}</div>
          </div>
          <div className="m-media-actions">
            <button
              type="button"
              className="m-media-btn"
              disabled={loading}
              onClick={async () => {
                const res = loaded ?? (await fetchMedia());
                if (res) {
                  const win = window.open(res.url, "_blank", "noopener");
                  if (!win) triggerMobileDownload(res.url, res.name || displayName);
                }
              }}
            >
              {loading ? <Loader2 size={12} className="spin" /> : <ExternalLink size={12} />}
              <span>查看</span>
            </button>
            <button
              type="button"
              className="m-media-icon-btn"
              aria-label="下载 PDF"
              onClick={async () => {
                const res = loaded ?? (await fetchMedia());
                if (res) triggerMobileDownload(res.url, res.name || displayName);
              }}
            >
              <Download size={14} />
            </button>
          </div>
        </div>
        {loading && pct !== undefined && (
          <div className="m-media-status">
            <span>加载中 {pct}%</span>
            <span className="m-media-bar">
              <span style={{ width: `${pct}%` }} />
            </span>
          </div>
        )}
        {error && <div className="m-media-error">{error}</div>}
      </div>
    );
  }

  if (mediaKind === "text") {
    return (
      <div className="m-media-card">
        <div className="m-media-head">
          <span className="m-media-badge text">
            <FileText size={15} />
          </span>
          <div className="m-media-meta">
            <div className="m-media-title">{titleText}</div>
            <div className="m-media-sub">
              {extBadge(displayName, "TXT")}
              {shownSize !== undefined ? ` · ${formatBytes(shownSize)}` : ""}
            </div>
          </div>
          <div className="m-media-actions">
            <button
              type="button"
              className="m-media-btn"
              disabled={loading}
              onClick={async () => {
                if (textOpen) {
                  setTextOpen(false);
                  return;
                }
                const res = loaded ?? (await fetchMedia());
                if (res) setTextOpen(true);
              }}
            >
              {loading ? <Loader2 size={12} className="spin" /> : <Eye size={12} />}
              <span>{textOpen ? "收起" : "预览"}</span>
            </button>
            <button
              type="button"
              className="m-media-icon-btn"
              aria-label="下载文件"
              onClick={async () => {
                const res = loaded ?? (await fetchMedia());
                if (res) triggerMobileDownload(res.url, res.name || displayName);
              }}
            >
              <Download size={14} />
            </button>
          </div>
        </div>
        {textOpen && loaded && (
          <div className="m-media-text-wrap">
            {loaded.text !== undefined ? (
              <>
                <div style={{ display: "flex", justifyContent: "flex-end", marginBottom: 4 }}>
                  <button
                    type="button"
                    className="m-media-btn"
                    onClick={() => {
                      navigator.clipboard?.writeText(loaded.text!).catch(() => {});
                      setCopied(true);
                      window.setTimeout(() => setCopied(false), 1500);
                    }}
                  >
                    {copied ? <Check size={12} /> : <Copy size={12} />}
                    <span>{copied ? "已复制" : "复制"}</span>
                  </button>
                </div>
                <pre className="m-media-text-pre mono">{loaded.text}</pre>
              </>
            ) : (
              <div className="m-media-status">文件较大或非文本编码，请点击下载查看</div>
            )}
          </div>
        )}
        {error && <div className="m-media-error">{error}</div>}
      </div>
    );
  }

  return (
    <div className="m-media-card">
      <div className="m-media-head">
        <span className="m-media-badge file">
          <FileIcon size={15} />
        </span>
        <div className="m-media-meta">
          <div className="m-media-title">{titleText}</div>
          <div className="m-media-sub">
            {extBadge(displayName, "FILE")}
            {shownSize !== undefined ? ` · ${formatBytes(shownSize)}` : ""}
          </div>
        </div>
        <div className="m-media-actions">
          <button
            type="button"
            className="m-media-btn"
            disabled={loading}
            onClick={async () => {
              const res = loaded ?? (await fetchMedia());
              if (res) triggerMobileDownload(res.url, res.name || displayName);
            }}
          >
            {loading ? <Loader2 size={12} className="spin" /> : <Download size={12} />}
            <span>下载</span>
          </button>
        </div>
      </div>
      {loading && pct !== undefined && (
        <div className="m-media-status">
          <span>下载中 {pct}%</span>
          <span className="m-media-bar">
            <span style={{ width: `${pct}%` }} />
          </span>
        </div>
      )}
      {error && <div className="m-media-error">{error}</div>}
    </div>
  );
}
