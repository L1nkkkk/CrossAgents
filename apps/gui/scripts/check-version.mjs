// A release ships the hub it was built with: the GUI's version (what electron-updater compares)
// and the hub's (what names `<home>/bin/<version>/`) move together. Run before packaging.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const gui = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8")).version;
const cargo = readFileSync(new URL("../../../Cargo.toml", import.meta.url), "utf8");
const hub = /\[workspace\.package\][^[]*?\nversion\s*=\s*"([^"]+)"/.exec(cargo)?.[1];
if (gui !== hub) {
  console.error(`version mismatch: apps/gui/package.json is ${gui}, Cargo.toml [workspace.package] is ${hub}; bump both`);
  process.exit(1);
}

if (process.argv.includes("--without-hub")) process.exit(0);

// The same path `build.win.extraResources` copies from. A release once went out without a hub at
// all: `cargo build --release` had written somewhere else (CARGO_TARGET_DIR was set in the
// shell), the source was simply missing, and electron-builder packaged the rest without
// complaining. The installer then had no hub to put in `<home>/bin/<version>/`, so an updated
// GUI kept talking to the old hub and a fresh install had none. Check the file is really
// there, and that it is this version.
const exe = fileURLToPath(new URL(`../../../target/release/crossagents${process.platform === "win32" ? ".exe" : ""}`, import.meta.url));
if (!existsSync(exe)) {
  console.error(`the hub binary is missing: ${exe}
build.win.extraResources copies it into the installer, and electron-builder does not fail when it is absent.
Build it into the repository's own target directory first:
  cargo build --release -p crossagents-cli
If CARGO_TARGET_DIR is set in this shell it goes elsewhere; unset it for that command.`);
  process.exit(1);
}

const reported = execFileSync(exe, ["--version"], { encoding: "utf8" }).trim();
if (!reported.endsWith(` ${hub}`)) {
  console.error(`the hub binary at ${exe} says "${reported}", not ${hub}; rebuild it before packaging`);
  process.exit(1);
}
