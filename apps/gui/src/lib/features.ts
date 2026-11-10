// What this GUI shows depends on what the hub says it has (docs/TODO.md §二). The hub lists
// the entries it has in `HubInfo.features`; a hub built before that list sends none, so its
// version is what says which of these entries it has — the table below. The sidebar's version
// tooltip and its notice say when an entry was inferred that way.
//
// A request the hub refuses with 403 (the credential's level is too low, docs/API-AUTH.md §3)
// greys the entry it came from instead of raising an error: the hub's own message is the reason.

import { useSyncExternalStore } from "react";

/**
 * Entry names the hub lists. Stable ids, also spelled out in `crates/api-types/src/lib.rs`
 * (`FEATURES`); the GUI ignores names it does not know, so a newer hub can add some.
 */
export const FEATURES = ["git", "lessons", "work", "apply", "devices", "link"] as const;
export type Feature = (typeof FEATURES)[number];

/** How an entry is named to a person, in the old-hub notice and the version tooltip. */
export const FEATURE_LABELS: Record<Feature, string> = {
  git: "Git 页",
  lessons: "排障记录",
  work: "工作组",
  apply: "应用到原目录",
  devices: "设备",
  link: "内置打洞",
};

/**
 * What a hub has when its `/v1/info` carries no feature list, read from the version it reports.
 *
 * The list arrived after 0.2.6, so a hub without it is at most 0.2.6 — but 0.2.6 itself already
 * shipped the Git page and 排障记录 (they were released in it). Hiding those on a 0.2.6 hub would
 * take away two pages the person can still use, which is why they are inferred here: upgrading
 * only the GUI must not lose working entries. 工作组、应用到原目录 和 设备 came after 0.2.6 and
 * are never inferred — without a list they stay hidden, whatever the version says.
 *
 * Rows are "from this version on"; a version below every row has none of these entries.
 */
export const WITHOUT_LIST: readonly { since: string; features: readonly Feature[] }[] = [
  { since: "0.2.6", features: ["git", "lessons"] },
];

/** Whether a hub version ("0.2.6", an optional `v` and suffix) is at least `min`. */
export function atLeast(hubVersion: string | undefined, min: string): boolean {
  const numbers = (v: string) =>
    (v.trim().replace(/^v/i, "").split(/[^0-9]+/).filter(Boolean).map(Number));
  const have = numbers(hubVersion ?? "");
  if (!have.length) return false;
  const want = numbers(min);
  for (let i = 0; i < want.length; i++) {
    const a = have[i] ?? 0;
    const b = want[i] ?? 0;
    if (a !== b) return a > b;
  }
  return true;
}

/**
 * What a 403 is kept for. Most are about a whole feature, but 设备's list is read while revoking
 * one is admin: a refusal of the revoke must not grey the entry that still lists them. The same
 * goes for agent settings and `repos/init`, which are admin since #retier while the rest of
 * their pages is write.
 */
export const OPERATIONS = ["devices.revoke", "agent.settings", "repos.init"] as const;
export type Operation = (typeof OPERATIONS)[number];
/** A key a 403 is filed under: a feature, or one operation inside a page. */
export type RefusalKey = Feature | Operation;

export type FeatureState = {
  /** The hub has answered `/v1/info` since the page opened. */
  known: boolean;
  /** The version it reported, for the inference a missing list needs. */
  version?: string;
  /**
   * The features it listed; `null` is a hub that predates the list. There is no way to tell
   * "an older hub" apart from "a hub with no features", so `WITHOUT_LIST` decides by version.
   */
  listed: readonly Feature[] | null;
  /** The hub's message for a 403, by feature or operation. */
  refused: Readonly<Partial<Record<RefusalKey, string>>>;
};

export type FeatureSupport = {
  /** The hub has answered; before that an entry is not shown yet, since it cannot be gated. */
  known: boolean;
  /** Show the entry at all: the hub listed it, or its version says it has it. */
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
  if (/^\/v1\/devices(?:\/|$)/.test(p)) return "devices";
  return undefined;
}

/**
 * What a refused request is about. One operation inside a page is kept apart from the page
 * itself, so a revoke that needs admin leaves the device list readable and only greys its own
 * button (#retier: agent settings and `repos/init` are admin too).
 */
export function operationOf(method: string, path: string): RefusalKey | undefined {
  const p = path.split("?")[0];
  if (method === "DELETE" && /^\/v1\/devices\/[^/]+$/.test(p)) return "devices.revoke";
  if (method === "POST" && /^\/v1\/agents\/[^/]+\/settings$/.test(p)) return "agent.settings";
  if (method === "POST" && p === "/v1/repos/init") return "repos.init";
  return featureOf(p);
}

const isFeature = (key: RefusalKey): key is Feature => (FEATURES as readonly string[]).includes(key);

/** What the hub has: its list, or — with none — what its version implies (`WITHOUT_LIST`). */
export function offeredOf(state: FeatureState): readonly Feature[] {
  if (state.listed !== null) return state.listed;
  if (!state.version) return [];
  return WITHOUT_LIST.filter((row) => atLeast(state.version, row.since)).flatMap((row) => row.features);
}

/** What an entry for `feature` should do in `state`. */
export function supportOf(state: FeatureState, feature: Feature): FeatureSupport {
  const known = state.known;
  if (!offeredOf(state).includes(feature)) return { known, show: false, disabled: false };
  const reason = state.refused[feature];
  return reason ? { known, show: true, disabled: true, reason } : { known, show: true, disabled: false };
}

/** The entries this hub leaves out, for the sidebar's explanation: empty when it has them all. */
export function hiddenByHub(state: FeatureState): Feature[] {
  if (!state.known) return [];
  const offered = offeredOf(state);
  return FEATURES.filter((f) => !offered.includes(f));
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
    ? `旧版 hub 没有提供功能清单，按版本 ${state.version ?? "未知"} 推断：${names} 不可用`
    : `这个 hub 没有这些功能：${names}`;
}

// ---- The page's copy of what the hub said: one store, updated by api.ts. ----

let state: FeatureState = INITIAL;
/** Bumped on every change; `useSyncExternalStore` reads it as the snapshot. */
let revision = 0;
const listeners = new Set<() => void>();

function publish(next: FeatureState) {
  state = next;
  revision += 1;
  listeners.forEach((l) => l());
}

/**
 * `/v1/info` answered: `features` is its list, or `undefined` from a hub that predates it, in
 * which case `version` decides what it has (`WITHOUT_LIST`). Called when a connection comes
 * up, so it also drops what an earlier connection refused.
 */
export function noteHub(features: readonly string[] | undefined, hubVersion: string | undefined) {
  const listed = features === undefined ? null : FEATURES.filter((f) => features.includes(f));
  publish({ known: true, version: hubVersion, listed, refused: {} });
}

/** A request answered 403: grey its entry (or its one operation), keeping the hub's message. */
export function noteForbidden(method: string, path: string, message: string) {
  const key = operationOf(method, path);
  if (!key) return;
  // A feature the hub does not list is hidden anyway, so its refusal changes nothing.
  if (isFeature(key) && !offeredOf(state).includes(key)) return;
  if (state.refused[key] === message) return;
  publish({ ...state, refused: { ...state.refused, [key]: message } });
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
  useSyncExternalStore(subscribe, () => revision);
  return supportOf(state, feature);
}

/** What an operation inside a page should do: greyed with the hub's reason after its 403. */
export type OperationSupport = { refused: boolean; reason?: string };

export function operationSupport(state: FeatureState, operation: Operation): OperationSupport {
  const reason = state.refused[operation];
  return reason ? { refused: true, reason } : { refused: false };
}

/** One request inside a page (设备 revoke, agent settings…), greyed after a 403 of its own. */
export function useOperation(operation: Operation): OperationSupport {
  useSyncExternalStore(subscribe, () => revision);
  return operationSupport(state, operation);
}

/** The whole state, when a component explains several entries at once (the version tooltip). */
export function useHubFeatures(): FeatureState {
  useSyncExternalStore(subscribe, () => revision);
  return state;
}
