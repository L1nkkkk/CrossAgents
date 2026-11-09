// A small single-colour mark for the system a machine runs. Drawn here as plain geometry
// (not the vendors' logos): a window of four panes, an apple-ish blob, a penguin-ish body, a
// phone, a robot head, and a generic computer for anything else. Colour follows `currentColor`.

import type { DeviceOs } from "@bindings/DeviceOs";
import type { TailscaleOs } from "./bridge";

export type OsKind = DeviceOs | TailscaleOs;

export const OS_NAMES: Record<OsKind, string> = {
  windows: "Windows",
  macos: "macOS",
  linux: "Linux",
  ios: "iOS",
  android: "Android",
  other: "系统未知",
};

/** The system's name; an unset or unknown os reads as unknown. */
export const osName = (os?: string): string => (os && os in OS_NAMES ? OS_NAMES[os as OsKind] : OS_NAMES.other);

export function OsIcon({ os, size = 14 }: { os?: string; size?: number }) {
  const kind: OsKind = os && os in OS_NAMES ? (os as OsKind) : "other";
  const name = OS_NAMES[kind];
  return (
    <svg className={`os-icon os-${kind}`} width={size} height={size} viewBox="0 0 16 16" fill="currentColor" role="img" aria-label={name} style={{ flexShrink: 0 }}>
      <title>{name}</title>
      {kind === "windows" && (
        <>
          <path d="M1.5 2.9 6.8 2.2v5.1H1.5z" />
          <path d="M7.5 2.1 14.5 1.1v6.2H7.5z" />
          <path d="M1.5 8.1h5.3v5.1l-5.3-.7z" />
          <path d="M7.5 8.1h7v6.8l-7-1z" />
        </>
      )}
      {kind === "macos" && (
        <>
          <path d="M8 4.6c-1-.8-3.9-.9-4.9 1.6-.9 2.3.2 5.2 1.5 6.6.8.9 1.7 1.1 2.1.9.5-.2.9-.4 1.3-.4s.8.2 1.3.4c.4.2 1.3 0 2-.8.6-.6 1-1.4 1.2-1.9-1.8-.8-2.2-3.4-.1-4.5-.9-1.2-2.8-1.4-3.6-.9z" />
          <path d="M8.4 3.9c0-1.3.9-2.4 2.2-2.6.1 1.3-.8 2.5-2.2 2.6z" />
        </>
      )}
      {kind === "linux" && (
        <>
          <path d="M8 1.5c-1.7 0-2.6 1.4-2.6 3.1 0 1.2-.4 1.9-1.1 3.1-.8 1.4-1.3 2.9-.8 4.1.5 1 1.8.9 2.6 1.1.4.8 1.2 1.1 1.9 1.1s1.5-.3 1.9-1.1c.8-.2 2.1-.1 2.6-1.1.5-1.2 0-2.7-.8-4.1-.7-1.2-1.1-1.9-1.1-3.1 0-1.7-.9-3.1-2.6-3.1z" />
          <circle cx="6.7" cy="4.4" r=".7" fill="var(--bg, #fff)" />
          <circle cx="9.3" cy="4.4" r=".7" fill="var(--bg, #fff)" />
        </>
      )}
      {kind === "ios" && (
        <>
          <path fillRule="evenodd" d="M5 1h6a1.5 1.5 0 0 1 1.5 1.5v11A1.5 1.5 0 0 1 11 15H5a1.5 1.5 0 0 1-1.5-1.5v-11A1.5 1.5 0 0 1 5 1zm.2 1.6v9.8h5.6V2.6z" />
          <circle cx="8" cy="13.6" r=".6" />
        </>
      )}
      {kind === "android" && (
        <>
          <path d="M3.2 9.5a4.8 4.8 0 0 1 9.6 0z" />
          <rect x="3.2" y="10.2" width="9.6" height="4" rx="1.2" />
          <path d="M5 3.2 6.2 5M11 3.2 9.8 5" stroke="currentColor" strokeWidth="1" strokeLinecap="round" />
          <circle cx="6.2" cy="7.6" r=".6" fill="var(--bg, #fff)" />
          <circle cx="9.8" cy="7.6" r=".6" fill="var(--bg, #fff)" />
        </>
      )}
      {kind === "other" && (
        <>
          <path fillRule="evenodd" d="M2.5 2.5h11A1 1 0 0 1 14.5 3.5v7a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1v-7a1 1 0 0 1 1-1zm.4 1.2v6.1h10.2V3.7z" />
          <path d="M5.5 13h5v1.2h-5z" />
          <path d="M7.4 11.5h1.2V13H7.4z" />
        </>
      )}
    </svg>
  );
}
