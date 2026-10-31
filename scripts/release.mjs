// 发一个版本：对齐版本号 → 提交 → 打 tag → 打包并发到 CrossAgents-releases → 推 main 和 tag。
//
// 用法：
//   node scripts/release.mjs              # 补丁位 +1（0.1.21 → 0.1.22）
//   node scripts/release.mjs 0.2.0        # 指定版本
//   node scripts/release.mjs --dry-run    # 只跑检查，什么都不改
//   node scripts/release.mjs --skip-publish  # 只提交和打 tag，不打包、不推（排查用）
//
// 顺序是故意这样排的：先在本地提交打 tag，打包发布成功之后才 push。发布失败时远端仍然干净，
// 删掉本地 tag 重来即可；反过来先 push 的话，远端会留下一个没有对应安装包的 tag。

import { execFileSync, execSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const RELEASES_REPO = "L1nkkkk/CrossAgents-releases";

const args = process.argv.slice(2);
const dryRun = args.includes("--dry-run");
const skipPublish = args.includes("--skip-publish");
const wanted = args.find((a) => !a.startsWith("--"));

function run(command, options = {}) {
  return execSync(command, { cwd: ROOT, encoding: "utf8", stdio: "pipe", ...options }).trim();
}

/** Shows its output as it goes: these are the long steps, and silence looks like a hang. */
function runLoud(command, options = {}) {
  execSync(command, { cwd: ROOT, stdio: "inherit", ...options });
}

function step(text) {
  console.log(`\n\x1b[36m▸ ${text}\x1b[0m`);
}

function die(message) {
  console.error(`\n\x1b[31m✗ ${message}\x1b[0m`);
  process.exit(1);
}

// `cargo build --release` has to write into the repository's own target/, because that is
// where build.extraResources copies the hub from. A release once went out with no hub at all
// because CARGO_TARGET_DIR was set in the shell: electron-builder packaged the rest without
// complaining, the installer had nothing to put in `<home>/bin/<version>/`, and an updated GUI
// kept talking to the old hub. check-version.mjs catches it now, but do not rely on that.
const buildEnv = { ...process.env };
delete buildEnv.CARGO_TARGET_DIR;

// ---------------------------------------------------------------------------- preflight

step("检查仓库状态");

const branch = run("git rev-parse --abbrev-ref HEAD");
if (branch !== "main") die(`当前在 ${branch}，发版要在 main 上`);

if (run("git status --porcelain")) {
  die("工作区有未提交的改动；发版提交只应包含版本号，先把它们处理掉");
}

// A release built from a commit nobody else has is fine; one built from a stale main is not.
run("git fetch origin main");
const behind = run("git rev-list --count HEAD..origin/main");
if (behind !== "0") die(`落后 origin/main ${behind} 个提交，先 git pull`);

const cargoPath = new URL("../Cargo.toml", import.meta.url);
const guiPkgPath = new URL("../apps/gui/package.json", import.meta.url);
const guiLockPath = new URL("../apps/gui/package-lock.json", import.meta.url);

const cargoToml = readFileSync(cargoPath, "utf8");
const current = /\[workspace\.package\][^[]*?\nversion\s*=\s*"([^"]+)"/.exec(cargoToml)?.[1];
if (!current) die("Cargo.toml 里找不到 [workspace.package] 的 version");

const guiVersion = JSON.parse(readFileSync(guiPkgPath, "utf8")).version;
if (guiVersion !== current) {
  die(`发版前两个版本号就对不上：Cargo.toml ${current}，apps/gui/package.json ${guiVersion}`);
}

const version = wanted ?? bumpPatch(current);
if (!/^\d+\.\d+\.\d+$/.test(version)) die(`版本号格式不对：${version}`);
if (version === current) die(`${version} 就是当前版本`);

const tag = `v${version}`;
if (run(`git tag --list ${tag}`)) die(`tag ${tag} 已存在；换个版本号，或先 git tag -d ${tag}`);
if (run(`git ls-remote --tags origin ${tag}`)) die(`origin 上已经有 tag ${tag}`);

// electron-builder 发到另一个仓库，要一个有 repo 权限的 token。gh 已经登录的话直接借它的。
let token = process.env.GH_TOKEN ?? process.env.GITHUB_TOKEN;
if (!token && !skipPublish) {
  try {
    token = execFileSync("gh", ["auth", "token"], { encoding: "utf8" }).trim();
  } catch {
    die("没有 GH_TOKEN，`gh auth token` 也拿不到；electron-builder 发不出去。先 gh auth login");
  }
}

console.log(`  ${current} → \x1b[1m${version}\x1b[0m（tag ${tag}，发到 ${RELEASES_REPO}）`);
console.log(`  HEAD ${run("git rev-parse --short HEAD")} ${run("git log -1 --format=%s")}`);

if (dryRun) {
  console.log("\n--dry-run：检查都过了，没有改动任何东西。");
  process.exit(0);
}

// ---------------------------------------------------------------------------- 改版本号

step(`写入版本号 ${version}`);

// 只替换 [workspace.package] 那一处，别碰依赖项自己的 version。
writeFileSync(
  cargoPath,
  cargoToml.replace(
    /(\[workspace\.package\][^[]*?\nversion\s*=\s*")[^"]+(")/,
    `$1${version}$2`,
  ),
);
bumpJson(guiPkgPath, current, version, 1);
// package-lock 里根对象和 packages[""] 各有一份。
bumpJson(guiLockPath, current, version, 2);
console.log("  Cargo.toml · apps/gui/package.json · apps/gui/package-lock.json");

// ---------------------------------------------------------------------------- 构建

step("cargo build --release -p crossagents-cli（顺带刷新 Cargo.lock）");
runLoud("cargo build --release -p crossagents-cli", { env: buildEnv });

const changed = run("git status --porcelain");
if (!changed.includes("Cargo.lock")) {
  console.log("  （Cargo.lock 没变化，继续）");
}

// ---------------------------------------------------------------------------- 提交打 tag

step(`提交并打 tag ${tag}`);
runLoud("git add Cargo.toml Cargo.lock apps/gui/package.json apps/gui/package-lock.json");
const message = `chore: 发布 ${version}\n\nCo-Authored-By: Claude Opus 5 <noreply@anthropic.com>\n`;
execFileSync("git", ["commit", "-F", "-"], { cwd: ROOT, input: message, stdio: ["pipe", "inherit", "inherit"] });
runLoud(`git tag ${tag}`);

if (skipPublish) {
  console.log(`\n--skip-publish：已提交并打好 ${tag}，没有打包也没有 push。`);
  console.log(`  继续： npm --prefix apps/gui run electron:release  然后  git push origin main ${tag}`);
  console.log(`  撤销： git tag -d ${tag} && git reset --hard HEAD~1`);
  process.exit(0);
}

// ---------------------------------------------------------------------------- 打包发布

step("打包并发到 " + RELEASES_REPO + "（几分钟）");
try {
  runLoud("npm run electron:release", {
    cwd: new URL("../apps/gui", import.meta.url),
    env: { ...buildEnv, GH_TOKEN: token },
  });
} catch {
  die(
    `打包或上传失败。远端还没有被改动过，本地回到发版前：\n` +
      `  git tag -d ${tag} && git reset --hard HEAD~1\n` +
      `修好之后重跑 node scripts/release.mjs ${version}`,
  );
}

// ---------------------------------------------------------------------------- push 与核对

step("推 main 和 tag");
runLoud(`git push origin main ${tag}`);

step("核对 " + RELEASES_REPO);
try {
  const view = execFileSync(
    "gh",
    ["release", "view", tag, "--repo", RELEASES_REPO, "--json", "name,isDraft,assets"],
    { encoding: "utf8", env: { ...process.env, GH_TOKEN: token } },
  );
  const release = JSON.parse(view);
  const assets = release.assets.map((a) => `${a.name} (${(a.size / 1048576).toFixed(1)} MB)`);
  console.log(`  ${release.name}${release.isDraft ? " \x1b[33m[草稿]\x1b[0m" : ""}`);
  for (const a of assets) console.log(`  · ${a}`);
  // electron-updater 靠 latest.yml 找新版本；缺了它，装好的 GUI 不会发现这一版。
  if (!release.assets.some((a) => a.name === "latest.yml")) {
    console.log("\n\x1b[33m⚠ 没有 latest.yml：已装的 GUI 不会自动发现这一版，去仓库里看看\x1b[0m");
  }
  if (release.isDraft) {
    console.log(`\n\x1b[33m⚠ 还是草稿，要手动发布：gh release edit ${tag} --repo ${RELEASES_REPO} --draft=false\x1b[0m`);
  }
} catch {
  console.log(`  读不到（权限或网络），自己看一眼： https://github.com/${RELEASES_REPO}/releases/tag/${tag}`);
}

console.log(`\n\x1b[32m✓ ${version} 发完了\x1b[0m`);

// ---------------------------------------------------------------------------- 小工具

function bumpPatch(v) {
  const [major, minor, patch] = v.split(".").map(Number);
  return `${major}.${minor}.${patch + 1}`;
}

/** 把 JSON 文件里的版本号换掉，并确认换到的处数和预期一致——少了多了都说明文件结构变了。 */
function bumpJson(path, from, to, expected) {
  const text = readFileSync(path, "utf8");
  const needle = `"version": "${from}"`;
  const hits = text.split(needle).length - 1;
  if (hits !== expected) {
    die(`${fileURLToPath(path)} 里 "version": "${from}" 出现了 ${hits} 次，预期 ${expected} 次；脚本不敢乱改`);
  }
  writeFileSync(path, text.split(needle).join(`"version": "${to}"`));
}
