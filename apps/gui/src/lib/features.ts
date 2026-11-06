// What this GUI shows depends on what the hub says it has (docs/TODO.md §二). The hub lists
// the entries it has in `HubInfo.features`; a hub built before that list sends nothing, and
// those entries stay hidden while the sidebar's version tooltip says why.
//
// A request the hub refuses with 403 (the credential's level is too low, docs/API-AUTH.md §3)
// greys the entry it came from instead of raising an error: the hub's own message is the reason.

import { useSyncExternalStore } from "react";

/**
 * Entry names the hub lists. Stable ids, also spelled out in `crates/api-types/src/lib.rs`
 * (`FEATURES`); the GUI ignores names it does not know, so a newer hub can add some.
 */
export const FEATURES = ["git", "lessons", "work", "apply"] as const;
export type Feature = (typeof FEATURES)[number];

/** How an entry is named to a person, in the old-hub notice and the version tooltip. */
export const FEATURE_LABELS: Record<Feature, string> = {
  git: "Git 页",
  lessons: "排障记录",
  work: "工作组",
  apply: "应用到原目录",
};

export type FeatureState = {
  /** The hub has answered `/v1/info` since the page opened. */
  known: boolean;
  /**
   * The features it listed; `null` is a hub that predates the list. There is no way to tell
   * "an older hub" apart from "a hub with no features", and either way the answer is hide.
   */
  listed: readonly Feature[] | null;
  /** The hub's message for a 403 from this entry, by feature. */
  refused: Readonly<Partial<Record<Feature, string>>>;
};

export type FeatureSupport = {
  /** The hub has answered; before that an entry is not shown yet, since it cannot be gated. */
  known: boolean;
  /** Show the entry at all: the hub listed it. */
  show: boolean;
  /** Keep the entry, greyed, with `reason` (the hub's message): it answered 403. */
  disabled: boolean;
  reason?: string;
};

export const INITIAL: FeatureState = { known: false, listed: null, refused: {} };

/** The feature an API path belongs to; `undefined` for what every hub has. */
export function featureOf(path: string): Feature | undefined {
  const p = path.split("?")[0];
  if (/^\/v1\/git(?:\/|$)/.test(p)) return "git";
  if (/^\/v1\/works?(?:\/|$)/.test(p)) return "work";
  if (/^\/v1\/lessons(?:\/|$)/.test(p) || /^\/v1\/projects\/[^/]+\/lessons(?:\/|$)/.test(p)) return "lessons";
  if (/^\/v1\/runs\/[^/]+\/(?:apply|integration)$/.test(p)) return "apply";
  return undefined;
}

/** What an entry for `feature` should do in `state`. */
export function supportOf(state: FeatureState, feature: Feature): FeatureSupport {
  const known = state.known;
  if (state.listed === null || !state.listed.includes(feature)) return { known, show: false, disabled: false };
  const reason = state.refused[feature];
  return reason ? { known, show: true, disabled: true, reason } : { known, show: true, disabled: false };
}

/** The entries this hub leaves out, for the sidebar's explanation: empty when it lists them all. */
export function hiddenByHub(state: FeatureState): Feature[] {
  if (!state.known) return [];
  return FEATURES.filter((f) => !state.listed?.includes(f));
}

/**
 * What the sidebar's version tooltip says about the entries this hub leaves out: the reason
 * they are hidden. Empty when nothing is hidden, or before the hub has answered.
 */
export function missingText(state: FeatureState): string {
  const hidden = hiddenByHub(state);
  if (!hidden.length) return "";
  const names = hidden.map((f) => FEATURE_LABELS[f]).join("、");
  return state.listed === null
    ? `旧版 hub 没有提供功能清单，这些入口已隐藏：${names}`
    : `这个 hub 没有这些功能：${names}`;
}

// ---- The page's copy of what the hub said: one store, updated by hub.ts and api.ts. ----

let state: FeatureState = INITIAL;
let version = 0;
const listeners = new Set<() => void>();

function publish(next: FeatureState) {
  state = next;
  version += 1;
  listeners.forEach((l) => l());
}

/**
 * `/v1/info` answered: `features` is its list, or `undefined` from a hub that predates it.
 * Called when a connection comes up, so it also drops what an earlier connection refused.
 */
export function noteHub(features: readonly string[] | undefined) {
  const listed = features === undefined ? null : FEATURES.filter((f) => features.includes(f));
  publish({ known: true, listed, refused: {} });
}

/** A request answered 403: grey its entry, keeping the hub's message as the reason. */
export function noteForbidden(path: string, message: string) {
  const feature = featureOf(path);
  if (!feature || !state.listed?.includes(feature) || state.refused[feature] === message) return;
  publish({ ...state, refused: { ...state.refused, [feature]: message } });
}

/** Forgets what the hub said, as when the page starts over. */
export function forgetHub() {
  publish(INITIAL);
}

/** The state as it is now, for tests and for `preview:mock` checks. */
export function currentState(): FeatureState {
  return state;
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** What an entry for `feature` should do now; re-renders when the hub's answer changes. */
export function useFeatureSupport(feature: Feature): FeatureSupport {
  useSyncExternalStore(subscribe, () => version);
  return supportOf(state, feature);
}

/** The whole state, when a component explains several entries at once (the version tooltip). */
export function useHubFeatures(): FeatureState {
  useSyncExternalStore(subscribe, () => version);
  return state;
}
