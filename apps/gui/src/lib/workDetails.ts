// The work details the sidebar reads (docs/tasks/G1.md §3.5). Listing the work groups is one
// request (`listWorks`); a group's rounds are a second one, asked for only when the person opens
// that row's fold. What was read is kept: opening the row again costs nothing unless a run or
// work event has said the rounds may have changed since (then it is reread once, on opening).
// While a row stays open, those events refresh it; while it is folded, they only mark it stale.
// So the number of requests follows what the person opened, not how many work groups exist.

import type { WorkDetail } from "@bindings/WorkDetail";

export type LoadWork = (id: string) => Promise<WorkDetail>;

export class WorkDetails {
  private readonly cache = new Map<string, { detail: WorkDetail; version: number }>();
  private readonly open = new Set<string>();
  private readonly pending = new Map<string, Promise<WorkDetail | undefined>>();
  private readonly listeners = new Set<() => void>();
  /** Bumped when an event says a round may have changed; a cache entry below it is stale. */
  private version = 0;

  /** What was read for this row, if anything. */
  get(id: string): WorkDetail | undefined {
    return this.cache.get(id)?.detail;
  }

  /** Cache changes, so an open row can show the rounds that just arrived. */
  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  /** A run or work event: cached rounds are now behind it; open ones are refreshed separately. */
  invalidate(): void {
    this.version += 1;
  }

  /** The row's rounds opened: show what was read before, and reread it only if it went stale. */
  expand(id: string, load: LoadWork): Promise<void> {
    this.open.add(id);
    const cached = this.cache.get(id);
    if (cached && cached.version === this.version) return Promise.resolve();
    return this.read(id, load).then(() => undefined);
  }

  /** The fold closed: stop refreshing this row; what was read stays in memory. */
  collapse(id: string): void {
    this.open.delete(id);
  }

  /** A run or work event: refresh the rows whose rounds are open, and only those. */
  refresh(load: LoadWork): Promise<void> {
    return Promise.all([...this.open].map((id) => this.read(id, load))).then(() => undefined);
  }

  private read(id: string, load: LoadWork): Promise<WorkDetail | undefined> {
    const running = this.pending.get(id);
    if (running) return running;
    const version = this.version;
    const request = load(id)
      .then((detail) => {
        this.cache.set(id, { detail, version });
        this.listeners.forEach((listener) => listener());
        return detail;
      })
      // A failed read leaves the row as it was; the work page reports the failure if opened.
      .catch(() => undefined)
      .finally(() => this.pending.delete(id));
    this.pending.set(id, request);
    return request;
  }
}
