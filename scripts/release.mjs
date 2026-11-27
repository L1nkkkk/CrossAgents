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
import { copyFileSync, existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const RELEASES_REPO = "L1nkkkk/CrossAgents-releases";
const TAURI_UPDATE_ENDPOINT = `https://github.com/${RELEASES_REPO}/releases/latest/download/latest.json`;
const TAURI_UPDATE_PUBKEY =
  "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEVEMDE3NTNCN0RGODYzRjgKUldUNFkvaDlPM1VCN1FJOGlSaVczK2dHNzluVEdoYzFsZ2QwcTI3UUE5cjloL2dndW0wL1JnS08K";

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
// where scripts/tauri-bundle.mjs copies the hub binaries from.
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

// 发到另一个仓库，要一个有 repo 权限的 token。gh 已经登录的话直接借它的。
let token = process.env.GH_TOKEN ?? process.env.GITHUB_TOKEN;
if (!token && !skipPublish) {
  try {
    token = execFileSync("gh", ["auth", "token"], { encoding: "utf8" }).trim();
  } catch {
    die("没有 GH_TOKEN，`gh auth token` 也拿不到；无法发布到 " + RELEASES_REPO + "。先 gh auth login");
  }
}

console.log(`  ${current} → \x1b[1m${version}\x1b[0m（tag ${tag}，发到 ${RELEASES_REPO}）`);
console.log(`  HEAD ${run("git rev-parse --short HEAD")} ${run("git log -1 --format=%s")}`);

// 发版前跑一遍全量 GUI 测试（含桌面路由白名单与 Tauri 打包配置校验）。
step("apps/gui 下 npm test（含打包与路由白名单测试）");
try {
  runLoud("npm test", { cwd: new URL("../apps/gui", import.meta.url), env: buildEnv });
} catch {
  die("npm test 没过；修好再发");
}

if (dryRun) {
  console.log("\n--dry-run：检查都过了，没有改动任何东西。");
  process.exit(0);
}

// ---------------------------------------------------------------------------- 改版本号

step(`写入版本号 ${version}`);

// 先算好、查过再写，出错时不留下只改了一半的文件。
const lockText = bumpedLock(readFileSync(guiLockPath, "utf8"), current, version);
// 只替换 [workspace.package] 那一处，别碰依赖项自己的 version。
writeFileSync(
  cargoPath,
  cargoToml.replace(
    /(\[workspace\.package\][^[]*?\nversion\s*=\s*")[^"]+(")/,
    `$1${version}$2`,
  ),
);
bumpJson(guiPkgPath, current, version, 1);
writeFileSync(guiLockPath, lockText);
console.log("  Cargo.toml · apps/gui/package.json · apps/gui/package-lock.json");

// ---------------------------------------------------------------------------- 构建

step("cargo build --release -p crossagents-cli -p crossagents-link（顺带刷新 Cargo.lock）");
runLoud("cargo build --release -p crossagents-cli -p crossagents-link", { env: buildEnv });

const changed = run("git status --porcelain");
if (!changed.includes("Cargo.lock")) {
  console.log("  （Cargo.lock 没变化，继续）");
}

// ---------------------------------------------------------------------------- 提交打 tag

step(`提交并打 tag ${tag}`);
runLoud("git add Cargo.toml Cargo.lock apps/gui/package.json apps/gui/package-lock.json");
const message = `chore: 发布 ${version}\n`;
execFileSync("git", ["commit", "-F", "-"], { cwd: ROOT, input: message, stdio: ["pipe", "inherit", "inherit"] });
runLoud(`git tag ${tag}`);

if (skipPublish) {
  console.log(`\n--skip-publish：已提交并打好 ${tag}，没有打包也没有 push。`);
  console.log(`  继续： npm --prefix apps/gui run tauri:bundle  然后上传并  git push origin main ${tag}`);
  console.log(`  撤销： git tag -d ${tag} && git reset --hard HEAD~1`);
  process.exit(0);
}

// ---------------------------------------------------------------------------- 打包发布

// 先把 release 建出来再上传产物，避免并发创建冲突或半成品上传。
step(`在 ${RELEASES_REPO} 建 ${tag}`);
const ghEnv = { ...process.env, GH_TOKEN: token };
try {
  execFileSync("gh", ["release", "view", tag, "--repo", RELEASES_REPO], { stdio: "pipe", env: ghEnv });
  console.log("  已存在，直接用");
} catch {
  execFileSync(
    "gh",
    ["release", "create", tag, "--repo", RELEASES_REPO, "--title", version, "--notes", `CrossAgents ${version}`],
    { stdio: "inherit", env: ghEnv },
  );
}

step("打包 Tauri 并生成更新清单（几分钟）");
const signingKeyPath = join(homedir(), ".tauri", "crossagents.key");
const signingKey = process.env.TAURI_SIGNING_PRIVATE_KEY ?? (existsSync(signingKeyPath) ? readFileSync(signingKeyPath, "utf8").trim() : "");
if (!signingKey) {
  die(`找不到 Tauri 签名私钥（TAURI_SIGNING_PRIVATE_KEY 或 ${signingKeyPath}）`);
}
const tauriEnv = {
  ...buildEnv,
  CROSSAGENTS_TAURI_UPDATE_ENDPOINT: TAURI_UPDATE_ENDPOINT,
  CROSSAGENTS_TAURI_UPDATE_PUBKEY: process.env.CROSSAGENTS_TAURI_UPDATE_PUBKEY ?? TAURI_UPDATE_PUBKEY,
  TAURI_SIGNING_PRIVATE_KEY: signingKey,
};
try {
  runLoud("npm run tauri:bundle", {
    cwd: new URL("../apps/gui", import.meta.url),
    env: tauriEnv,
  });
} catch {
  die(
    `打包失败。远端还没有被改动过，本地回到发版前：\n` +
      `  git tag -d ${tag} && git reset --hard HEAD~1\n` +
      `修好之后重跑 node scripts/release.mjs ${version}`,
  );
}

const nsisDir = fileURLToPath(new URL("../apps/gui/src-tauri/target/release/bundle/nsis", import.meta.url));
const nsisExe = readdirSync(nsisDir).find((f) => f.endsWith(`_${version}_x64-setup.exe`));
if (!nsisExe || !existsSync(join(nsisDir, `${nsisExe}.sig`))) {
  die(`在 ${nsisDir} 找不到签名后的 NSIS 安装包或 .sig 文件`);
}
const assetName = `CrossAgents-Tauri-${version}-x64-setup.exe`;
const assetPath = join(nsisDir, assetName);
const sigPath = `${assetPath}.sig`;
if (nsisExe !== assetName) {
  copyFileSync(join(nsisDir, nsisExe), assetPath);
  copyFileSync(join(nsisDir, `${nsisExe}.sig`), sigPath);
}
const manifestPath = join(nsisDir, "latest.json");
const manifest = execFileSync(
  process.execPath,
  [
    "scripts/tauri-update-manifest.mjs",
    `https://github.com/${RELEASES_REPO}/releases/download/${tag}/`,
    `windows-x86_64=${assetPath}`,
  ],
  { cwd: fileURLToPath(new URL("../apps/gui", import.meta.url)), encoding: "utf8" },
);
writeFileSync(manifestPath, manifest);

step("上传到 " + RELEASES_REPO);
try {
  execFileSync(
    "gh",
    ["release", "upload", tag, assetPath, sigPath, manifestPath, "--repo", RELEASES_REPO, "--clobber"],
    { stdio: "inherit", env: ghEnv },
  );
} catch {
  die(
    `上传失败。远端还没有推 main/tag，本地回到发版前：\n` +
      `  git tag -d ${tag} && git reset --hard HEAD~1`,
  );
}

// ---------------------------------------------------------------------------- 核对与 push

// 核对在 push 之前：发不全的时候，源仓库还没留下一个指向半成品的 tag。
step("核对 " + RELEASES_REPO);
const view = execFileSync(
  "gh",
  ["release", "view", tag, "--repo", RELEASES_REPO, "--json", "name,isDraft,assets"],
  { encoding: "utf8", env: ghEnv },
);
const release = JSON.parse(view);
for (const a of release.assets) console.log(`  · ${a.name} (${(a.size / 1048576).toFixed(1)} MB)`);

const want = [assetName, `${assetName}.sig`, "latest.json"];
const missing = want.filter((w) => !release.assets.some((a) => a.name === w));
if (missing.length) {
  die(`release 缺少：${missing.join("、")}`);
}
if (release.isDraft) {
  die(`还是草稿，没人能下载：gh release edit ${tag} --repo ${RELEASES_REPO} --draft=false`);
}

step("推 main 和 tag");
runLoud(`git push origin main ${tag}`);

console.log(`\n\x1b[32m✓ ${version} 发完了\x1b[0m`);
console.log(`  https://github.com/${RELEASES_REPO}/releases/tag/${tag}`);

function bumpPatch(v) {
  const [major, minor, patch] = v.split(".").map(Number);
  return `${major}.${minor}.${patch + 1}`;
}

/** 把 JSON 文件里的版本号换掉，并确认换到的处数和预期一致——少了多了都说明文件结构变了。 */
/**
 * package-lock 里属于本项目的版本号只有根对象和 packages[""] 两处，都在文件开头；依赖可能恰好
 * 同版本（0.2.0 的 chromium-pickle-js），不能动。改完确认除这两处外内容没变。
 */
function bumpedLock(text, from, to) {
  const before = JSON.parse(text);
  if (before.version !== from || before.packages?.[""]?.version !== from) {
    die(`package-lock.json 的版本号不是 ${from}：根对象 ${before.version}，packages[""] ${before.packages?.[""]?.version}`);
  }
  let left = 2;
  const next = text.replaceAll(`"version": "${from}"`, (hit) => (left-- > 0 ? `"version": "${to}"` : hit));
  const after = JSON.parse(next);
  const same = after.version === to && after.packages[""].version === to;
  after.version = from;
  after.packages[""].version = from;
  if (!same || JSON.stringify(after) !== JSON.stringify(before)) {
    die("package-lock.json 改版本号时碰到了别的内容；脚本不敢继续");
  }
  return next;
}

function bumpJson(path, from, to, expected) {
  const text = readFileSync(path, "utf8");
  const needle = `"version": "${from}"`;
  const hits = text.split(needle).length - 1;
  if (hits !== expected) {
    die(`${fileURLToPath(path)} 里 "version": "${from}" 出现了 ${hits} 次，预期 ${expected} 次；脚本不敢乱改`);
  }
  writeFileSync(path, text.split(needle).join(`"version": "${to}"`));
}
