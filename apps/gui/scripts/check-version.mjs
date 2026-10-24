// A release ships the hub it was built with: the GUI's version (what electron-updater compares)
// and the hub's (what names `<home>/bin/<version>/`) move together. Run before packaging.

import { readFileSync } from "node:fs";

const gui = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8")).version;
const cargo = readFileSync(new URL("../../../Cargo.toml", import.meta.url), "utf8");
const hub = /\[workspace\.package\][^[]*?\nversion\s*=\s*"([^"]+)"/.exec(cargo)?.[1];
if (gui !== hub) {
  console.error(`version mismatch: apps/gui/package.json is ${gui}, Cargo.toml [workspace.package] is ${hub}; bump both`);
  process.exit(1);
}
