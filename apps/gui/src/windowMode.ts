// Three QQ-style desktop window modes:
// - "efficiency": full multi-pane workspace (default)
// - "classic": narrow vertical QQ-style floating session panel (with quick in-pane chat)
// - "mini": ultra-compact always-on-top gaming HUD (collapsible pill + opacity/auto-fade)

import { useEffect, useSyncExternalStore } from "react";
import type { WindowMode, WindowModeSnapshot } from "./bridge";

export type { WindowMode };

export type WindowModeView = {
  mode: WindowMode;
  alwaysOnTop: boolean;
  opacity: number;
  autoFade: boolean;
  miniCollapsed: boolean;
};

const STORAGE_KEY = "crossagents.windowMode.ui";

function readLocal(): Partial<WindowModeView> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as Partial<WindowModeView>;
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

function writeLocal(state: WindowModeView) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    // Private browsing or full storage.
  }
}

const stored = readLocal();
let current: WindowModeView = {
  mode: stored.mode === "classic" || stored.mode === "mini" ? stored.mode : "efficiency",
  alwaysOnTop: Boolean(stored.alwaysOnTop),
  opacity: typeof stored.opacity === "number" ? Math.min(1, Math.max(0.35, stored.opacity)) : 1,
  autoFade: stored.autoFade ?? true,
  miniCollapsed: Boolean(stored.miniCollapsed),
};

const listeners = new Set<() => void>();
let bridgeSubscribed = false;
let hovering = false;

function emit(next: WindowModeView) {
  current = next;
  writeLocal(next);
  listeners.forEach((l) => l());
}

function applySnapshot(snap: WindowModeSnapshot) {
  emit({
    ...current,
    mode: snap.mode,
    alwaysOnTop: snap.alwaysOnTop,
    opacity: snap.opacity,
    miniCollapsed: snap.miniCollapsed,
  });
}

function bridge() {
  return typeof window !== "undefined" ? window.crossagents?.windowMode : undefined;
}

function ensureBridgeSync() {
  if (bridgeSubscribed) return;
  const api = bridge();
  if (!api) return;
  bridgeSubscribed = true;
  api.get().then(applySnapshot, () => {});
  api.onChange(applySnapshot);
}

export async function setWindowMode(mode: WindowMode): Promise<void> {
  const api = bridge();
  if (api) {
    try {
      const snap = await api.set(mode);
      applySnapshot(snap);
      return;
    } catch {
      // Fall through to local update for browser/preview.
    }
  }
  emit({
    ...current,
    mode,
    alwaysOnTop: mode === "mini" ? true : mode === "classic" ? current.alwaysOnTop : false,
    opacity: mode === "efficiency" ? 1 : mode === "mini" ? Math.min(current.opacity, 0.94) : current.opacity,
  });
}

export async function setWindowAlwaysOnTop(enabled: boolean): Promise<void> {
  const api = bridge();
  if (api) {
    try {
      const snap = await api.setAlwaysOnTop(enabled);
      applySnapshot(snap);
      return;
    } catch {
      // ignore
    }
  }
  emit({ ...current, alwaysOnTop: enabled });
}

export async function setWindowOpacity(opacity: number, persist = true): Promise<void> {
  const clamped = Math.min(1, Math.max(0.35, Number(opacity.toFixed(2))));
  const api = bridge();
  if (api) {
    try {
      const snap = await api.setOpacity(clamped, persist);
      if (persist) applySnapshot(snap);
      return;
    } catch {
      // ignore
    }
  }
  if (persist) emit({ ...current, opacity: clamped });
}

export function setWindowAutoFade(enabled: boolean): void {
  emit({ ...current, autoFade: enabled });
  if (!enabled) {
    void setWindowOpacity(current.opacity, false);
  }
}

export async function setMiniCollapsed(collapsed: boolean): Promise<void> {
  const api = bridge();
  if (api) {
    try {
      const snap = await api.setMiniCollapsed(collapsed);
      applySnapshot(snap);
      return;
    } catch {
      // ignore
    }
  }
  emit({ ...current, miniCollapsed: collapsed });
}

/** Cycles preset opacities in Mini/Classic mode: 100% → 85% → 65% → 45% → 100%. */
export function cycleWindowOpacity(): void {
  const presets = [1, 0.85, 0.65, 0.45];
  const next = presets.find((p) => p < current.opacity - 0.04) ?? 1;
  void setWindowOpacity(next, true);
}

/** Reactive hook for the active window mode and gaming HUD preferences. */
export function useWindowMode(): WindowModeView {
  useEffect(() => {
    ensureBridgeSync();
  }, []);

  const state = useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => current,
  );

  // Auto-fade when blurred in Mini/Classic mode, snapping back to 100% when hovered or focused.
  useEffect(() => {
    const api = bridge();
    if (!api || state.mode === "efficiency" || !state.autoFade) return;
    const applyFocusOpacity = () => {
      const clear = hovering || document.hasFocus();
      const target = clear ? 1 : Math.min(state.opacity, 0.72);
      void api.setOpacity(target, false).catch(() => {});
    };
    const onEnter = () => {
      hovering = true;
      applyFocusOpacity();
    };
    const onLeave = () => {
      hovering = false;
      applyFocusOpacity();
    };
    window.addEventListener("focus", applyFocusOpacity);
    window.addEventListener("blur", applyFocusOpacity);
    document.documentElement.addEventListener("mouseenter", onEnter);
    document.documentElement.addEventListener("mouseleave", onLeave);
    return () => {
      window.removeEventListener("focus", applyFocusOpacity);
      window.removeEventListener("blur", applyFocusOpacity);
      document.documentElement.removeEventListener("mouseenter", onEnter);
      document.documentElement.removeEventListener("mouseleave", onLeave);
    };
  }, [state.mode, state.autoFade, state.opacity]);

  return state;
}
