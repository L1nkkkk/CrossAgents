// Inline code that looks like a path becomes a link only once the hub says it exists. The
// questions are batched (one `files/resolve` per scope per tick, at most 200 paths) and the
// answers cached per scope, so a long transcript asks once per distinct path.
import { useEffect, useState } from "react";
import { api, scopeKey, type FileScope } from "./api";

/** The workspace-relative path, or null when it does not exist (or is outside). */
type Answer = string | null;

const MAX = 200;
const cache = new Map<string, Map<string, Answer>>();
const waiting = new Map<string, { scope: FileScope; paths: Map<string, ((a: Answer) => void)[]> }>();
let timer: ReturnType<typeof setTimeout> | undefined;

function flush() {
  timer = undefined;
  const batches = [...waiting.values()];
  waiting.clear();
  for (const { scope, paths } of batches) {
    const key = scopeKey(scope);
    const all = [...paths.keys()];
    for (let i = 0; i < all.length; i += MAX) {
      const chunk = all.slice(i, i + MAX);
      const settle = (answers: Answer[]) => {
        const known = cache.get(key) ?? new Map();
        cache.set(key, known);
        chunk.forEach((p, j) => {
          known.set(p, answers[j]);
          paths.get(p)?.forEach((cb) => cb(answers[j]));
        });
      };
      api.filesResolve(scope, chunk).then(
        (r) => settle(chunk.map((_, j) => (r.entries[j]?.path ? r.entries[j].path! : null))),
        // An error (no workspace, offline) is not cached: the next render asks again.
        () => chunk.forEach((p) => paths.get(p)?.forEach((cb) => cb(null))),
      );
    }
  }
}

/** The cached answer, if any (undefined: not asked yet). */
export function cachedResolve(scope: FileScope, path: string): Answer | undefined {
  return cache.get(scopeKey(scope))?.get(path);
}

export function resolvePath(scope: FileScope, path: string): Promise<Answer> {
  const hit = cachedResolve(scope, path);
  if (hit !== undefined) return Promise.resolve(hit);
  return new Promise((resolve) => {
    const key = scopeKey(scope);
    const batch = waiting.get(key) ?? { scope, paths: new Map() };
    waiting.set(key, batch);
    const list = batch.paths.get(path) ?? [];
    list.push(resolve);
    batch.paths.set(path, list);
    timer ??= setTimeout(flush, 0);
  });
}

/** Whether `path` exists in the scope's workspace: the relative path, null, or undefined while asking. */
export function useResolved(scope: FileScope | null, path: string | undefined): Answer | undefined {
  const initial = scope && path ? cachedResolve(scope, path) : null;
  const [answer, setAnswer] = useState<Answer | undefined>(initial);
  useEffect(() => {
    if (!scope || !path) {
      setAnswer(null);
      return;
    }
    const hit = cachedResolve(scope, path);
    if (hit !== undefined) {
      setAnswer(hit);
      return;
    }
    let live = true;
    setAnswer(undefined);
    resolvePath(scope, path).then((a) => live && setAnswer(a));
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope && scopeKey(scope), path]);
  return answer;
}
