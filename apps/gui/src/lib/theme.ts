// The colour theme (docs/tasks/V6-GUI.md §4.4): follow the system, or always light / dark; light
// comes in three papers. Kept in localStorage `crossagents.theme`; applied as
// `<html data-theme="light | light-paper | light-mist | dark">`, which styles.css keys on.
// 墨 is 素白 with ink instead of indigo for the accent, in light and dark alike: it adds
// `<html data-accent="ink">` on top of the plain theme.

import { useSyncExternalStore } from "react";
import { getBridge } from "../api";

export type Mode = "system" | "light" | "dark";
export type Paper = "plain" | "paper" | "mist" | "ink";
export type ThemeChoice = { mode: Mode; paper: Paper };
export type Applied = "light" | "light-paper" | "light-mist" | "dark";

export const MODES: { key: Mode; label: string }[] = [
  { key: "system", label: "跟随系统" },
  { key: "light", label: "浅色" },
  { key: "dark", label: "深色" },
];

export const PAPERS: { key: Paper; label: string; hint: string; swatch: string; card?: string }[] = [
  { key: "plain", label: "素白", hint: "中性灰白", swatch: "#f4f4f2" },
  { key: "paper", label: "纸", hint: "偏暖，像纸", swatch: "#faf9f5" },
  { key: "mist", label: "雾", hint: "偏冷的灰", swatch: "#f3f4f6" },
  { key: "ink", label: "墨", hint: "素白底，主操作用墨色不用靛紫；深色下也生效", swatch: "#f4f4f2", card: "#18181b" },
];

const KEY = "crossagents.theme";
const DEFAULT: ThemeChoice = { mode: "system", paper: "plain" };
const listeners = new Set<() => void>();
let choice: ThemeChoice = load();
let started = false;

function load(): ThemeChoice {
  try {
    const saved = JSON.parse(localStorage.getItem(KEY) ?? "null");
    const mode = MODES.some((m) => m.key === saved?.mode) ? saved.mode : DEFAULT.mode;
    const paper = PAPERS.some((p) => p.key === saved?.paper) ? saved.paper : DEFAULT.paper;
    return { mode, paper };
  } catch {
    return DEFAULT;
  }
}

const dark = () => typeof matchMedia !== "undefined" && matchMedia("(prefers-color-scheme: dark)").matches;

/** What the choice shows right now. */
export function resolve(c: ThemeChoice, systemDark: boolean): Applied {
  const isDark = c.mode === "dark" || (c.mode === "system" && systemDark);
  if (isDark) return "dark";
  return c.paper === "plain" || c.paper === "ink" ? "light" : `light-${c.paper}`;
}

function apply() {
  const applied = resolve(choice, dark());
  document.documentElement.dataset.theme = applied;
  if (choice.paper === "ink") document.documentElement.dataset.accent = "ink";
  else delete document.documentElement.dataset.accent;
  // The window frame follows (Electron); read the colours the stylesheet now gives.
  const css = getComputedStyle(document.documentElement);
  getBridge()?.setTheme?.({
    dark: applied === "dark",
    background: css.getPropertyValue("--page").trim(),
    foreground: css.getPropertyValue("--text").trim(),
  });
}

/** Applies the saved theme and follows the system's while the choice is "跟随系统". */
export function startTheme() {
  if (started) return;
  started = true;
  apply();
  matchMedia?.("(prefers-color-scheme: dark)").addEventListener?.("change", () => {
    if (choice.mode === "system") apply();
  });
  // Another window of the app (or the mock preview's frame) changed it.
  window.addEventListener("storage", (e) => {
    if (e.key !== KEY) return;
    choice = load();
    apply();
    listeners.forEach((l) => l());
  });
}

export function setTheme(next: Partial<ThemeChoice>) {
  choice = { ...choice, ...next };
  try {
    localStorage.setItem(KEY, JSON.stringify(choice));
  } catch {
    // Lasts for this page only.
  }
  apply();
  listeners.forEach((l) => l());
}

export const useTheme = () =>
  useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => choice,
  );
