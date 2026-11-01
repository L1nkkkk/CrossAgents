// Build-time constants (vite.config.ts `define`).

/** True only in `npm run preview:mock`; other builds drop everything behind it. */
declare const __MOCK__: boolean;
/** What the page was built from: package version, git commit, and whether it is the dev server. */
declare const __BUILD__: { version: string; commit?: string; dev: boolean };

/** Vite gives an imported image as its URL. */
declare module "*.svg" {
  const url: string;
  export default url;
}
