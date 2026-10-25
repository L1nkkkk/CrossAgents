// What the page and the hub were built from, so the sidebar can tell when they differ. Pure,
// tested in build.test.ts.

/** The page's build (vite.config.ts `__BUILD__`). */
export type PageBuild = { version: string; commit?: string; dev: boolean };
/** The hub's build, from `/v1/info`; `commit` and `debug` are missing on older hubs. */
export type HubBuild = { version: string; commit?: string; debug?: boolean };

/** "v0.1.0 · d5011e4", or just the version without a commit. */
export function buildTag(b: { version: string; commit?: string }): string {
  return b.commit ? `v${b.version} · ${b.commit}` : `v${b.version}`;
}

/**
 * Whether page and hub come from the same build: same version, and the same commit when both
 * know theirs. `short` is what tells them apart in a few characters (the commits when the
 * versions match, else the versions).
 */
export function compareBuilds(page: PageBuild, hub: HubBuild): { same: boolean; page: string; hub: string } {
  const sameVersion = page.version === hub.version;
  const same = sameVersion && (!page.commit || !hub.commit || page.commit === hub.commit);
  const short = (b: { version: string; commit?: string }) => (sameVersion && b.commit ? b.commit : `v${b.version}`);
  return { same, page: short(page), hub: short(hub) };
}
