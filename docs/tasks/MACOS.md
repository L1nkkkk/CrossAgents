# macOS 任务书：让项目在 Mac 上跑起来

> 来源：2026-11-07 讨论（run 4c111086）· 相关设计稿：[MULTI-NODE.md](../MULTI-NODE.md) §7.1、§12、§15 · [EXTENSIONS.md](../EXTENSIONS.md) §9「浏览器视图」· 状态：只有设计，未开工 · 2026-11-07

## 1. 现状

项目从没在 macOS 上跑过。三件事都缺：装得起来、界面上的 Mac 习惯、跑得对。

- **打包只有 Windows**。[apps/gui/package.json](../../apps/gui/package.json) 的 `electron:build` / `electron:release` 写死 `--win`，`build` 里只有 `win`（target `nsis`），`extraResources` 写死 `crossagents.exe` 和 `icon.ico`。[U1.md](U1.md) §4 也记了「打包只做了 Windows」。所以 Mac 上现在没有任何安装包，连不签名的 dmg 都没有。
- **没有 CI**：仓库没有 `.github/`，macOS 上的代码一次都没被编译、测试过。这是最便宜、也最先该补的一环——它不依赖有没有 Mac 机器。
- **Unix 代码是真实现，但没跑过**：宿主在非 Windows 上用进程组代替 Job Object，`ps` 查占用、Unix 版测试脚本都写了（[PROCESS-SUPERVISION.md](../PROCESS-SUPERVISION.md) §7）。[TODO.md](../TODO.md) §一「Linux / macOS 上实测宿主」推迟到 B1，[LOCKS.md](LOCKS.md) §5 的 Unix 实验（`flock` 继承、`setsid` 脱离、进程组前任检查）**一次都没跑过**，现在只能按弱保证措辞。
- **已经为 macOS 准备好的**（对照代码核过）：`main.cts` 的 `titleBarStyle: "hiddenInset"` 与 `trafficLightPosition`；`styles.css` 的 `.titlebar.mac`（`padding-left: 80px`，`src/titlebar.tsx` 按 `navigator.userAgent` 加这个类）；`window-all-closed` 区分 `darwin`；`RUNS_WHEN_OPENED` 已含 `.app` / `.command` / `.sh`；部分快捷键已认 `metaKey`（[App.tsx](../../apps/gui/src/App.tsx) 的 `mod`、[SessionComposer.tsx](../../apps/gui/src/views/SessionComposer.tsx) 的 `composerAction`、[Sidebar.tsx](../../apps/gui/src/Sidebar.tsx) 的旁开、[menubar.tsx](../../apps/gui/src/menubar.tsx) 里 `Ctrl+/` 的监听）；`main.cts` 起 hub 时已按平台选 `crossagents` / `crossagents.exe`；页内菜单栏在 Mac 上已经隐藏（`titlebar.tsx` 的 `{!isMac && <MenuBar />}`）。

最后一条值得单独说：Mac 上页内菜单栏不显示，而应用菜单又被 `Menu.setApplicationMenu(null)` 去掉了，所以现在 Mac 上一个菜单都没有。这是 §2 第一条要补的东西，不是「再隐藏一次」。

## 2. GUI 要改的

每条都写文件和符号，行号会漂移，不写。

**应用菜单**。[apps/gui/electron/main.cts](../../apps/gui/electron/main.cts) 的 `Menu.setApplicationMenu(null)` 上方注释说 "Copy and paste still work."——这句在 Windows 上成立，因为 Chromium 自己处理了这些编辑键。macOS 上不是：⌘C / ⌘V / ⌘X / ⌘A / ⌘Z、⌘Q、⌘M 都由应用菜单提供，菜单为空就失效（Electron 已知行为，**要在 Mac 上确认**）。改法：macOS 用 `Menu.buildFromTemplate` 建一份原生菜单，至少「应用」（`role: "appMenu"`，带退出与隐藏）、「编辑」（`role` 用 `undo` / `redo` / `cut` / `copy` / `paste` / `selectAll`）、「窗口」（`minimize` / `zoom` / `front`）；其它平台保持 `null`。页内菜单栏在 Mac 上已经不显示（见 §1），不需要再动。

**快捷键**。同一文件的 `before-input-event`：缩放那一段判断 `!input.control`，dev server 那一段（`F12`、`input.control && input.shift && i`、`input.control && r`）也只认 `control`。Mac 上要认 `input.meta`——⌘= / ⌘- / ⌘0 是缩放，⌘⌥I 是开发者工具，⌘R 是重新加载。同时 [menubar.tsx](../../apps/gui/src/menubar.tsx) 里每条 `Entry` 的 `keys: "Ctrl+…"` 和 `SHORTCUTS` 表都是写死的中文串，按平台显示 ⌘：菜单栏在 Mac 上不显示，但「键盘快捷键」对话框（`Ctrl+/` 打开，这个监听已认 `metaKey`）仍会列出 `Ctrl+`。

**点 Dock 图标**。`main.cts` 的 `app.on("activate")` 只在 `BrowserWindow.getAllWindows().length === 0` 时 `createWindow()`。但托盘把窗口藏起来而不是关掉：[tray.cts](../../apps/gui/electron/tray.cts) 的 `keep(win)` 在 `close` 里 `event.preventDefault()` 再 `win.hide()`，窗口一直在。于是 Mac 上点 Dock 图标什么都不发生，窗口回不来。改成调已经有的 `showWindow()`（现在 `second-instance` 在用）——它没有窗口时也会 `createWindow()`，两种情形都对。

**托盘**。`main.cts` 里 `createTray(join(process.resourcesPath, "icon.ico"), …)` 写死 `.ico`；macOS 菜单栏要模板 PNG——黑色图形、透明背景，系统按深浅色反色——`nativeImage` 上设 `setTemplateImage(true)` 或文件名以 `Template` 结尾。`tray.cts` 的 `displayBalloon` 和 `balloon-click` 只在 Windows 有效，Mac 上要改用系统通知（`Notification`）。`hintOnce` 的文案「点任务栏右下角的图标可以重新打开」按平台改（Mac 是菜单栏图标）。`Tray` 的 `click` / `right-click` 在 Mac 上的语义也要一并核对（**要在 Mac 上确认**）。

**打开即执行的文件**。`main.cts` 的 `RUNS_WHEN_OPENED` 补 macOS 的 `.terminal`、`.fileloc`——这两个在 Mac 上会被系统当成可执行/定位文件打开，和 `.app` / `.command` / `.sh` 是一类。

## 3. 打包与分发

**electron-builder 加 mac 目标**：`dmg`（安装）加 `zip`（自动更新用），图标 `build/icon.icns`（现在 `build/` 里只有 `icon.ico`、`icon.png`）。`extraResources` 不能再写死 `crossagents.exe`：hub 二进制按 `aarch64-apple-darwin` 和 `x86_64-apple-darwin` 各编一份，再 `lipo` 成通用版放进 `resources/bin/`。`main.cts` 已经按平台取 `crossagents`，这一半不用改。

**摄像头 / 麦克风**：浏览器视图（[EXTENSIONS.md](../EXTENSIONS.md) §9「浏览器视图」）要开摄像头或麦克风时，macOS 必须在 `Info.plist` 里声明 `NSCameraUsageDescription` / `NSMicrophoneUsageDescription`，并带对应的 entitlement；不声明会出问题，具体表现要在 Mac 上确认。主窗口的权限仍由 `permissions.cts` 的 `permissionAllowed` 决定（只放行 `clipboard-sanitized-write`），这条不变。

**签名与公证**：Apple Developer ID 证书、hardened runtime，`bin/` 里的 hub 二进制要一起签名、一起公证。不签名的后果，照原话：首次打开要去「系统设置 → 隐私与安全性」放行；`electron-updater` 在 macOS 上要求签名，不签就没有自动更新。

**只能在 macOS 上做**：签名和公证都只能在 macOS 上跑，所以要么有一台 Mac，要么用 macOS 的 CI runner；私有仓库的 runner 要计费（见 §6）。这一条是 §5 里 Mac4 的硬依赖。

## 4. 节点与 hub 在 macOS 上

**PATH 是最大的坑**。从 Finder、Dock 或 launchd 启动的进程只有 `/usr/bin:/bin:/usr/sbin:/sbin`；GUI 起 hub 时原样继承——[localHub.cts](../../apps/gui/electron/localHub.cts) 的 `run` 用 `execFile(exe, args, { timeout, windowsHide: true })`，不传 `env`，所以 `process.env` 整个传下去，包括那个残缺的 `PATH`。后果是 npm / Homebrew 装的 CLI 找不到、agent 要用的 `cargo` / `npm` 找不到、扩展工具要的 `node` 找不到。Windows 上靠读注册表绕开（[DESIGN.md](../DESIGN.md) §9.3「hub 的 PATH 是旧的」那段：合并 `HKCU\Environment` 与系统 `PATH`）；macOS 没有注册表，要用登录 shell 取环境——做法见 [MULTI-NODE.md](../MULTI-NODE.md) §7.1（节点启动时用登录 shell 取环境），单机时同样适用（[MULTI-NODE.md](../MULTI-NODE.md) §15 单干模式把本机 hub 也当成一个内置节点，出处是 MULTI-NODE §12 B1 那句「单机模式就是 hub 加一个内置节点」）。

**内置终端的 shell**。[crates/hub/src/terminal.rs](../../crates/hub/src/terminal.rs) 的 `default_shell`（非 Windows 分支）用 `$SHELL`，没设时 `/bin/sh`，不带参数。macOS 的惯例是开登录 shell（`-l`），否则 `~/.zprofile` 里的 Homebrew 路径不生效，用户在内置终端里敲 `brew` 就是 command not found。

**CLI 查找**。[crates/plugins/agents/](../../crates/plugins/agents/) 下的描述文件里，按系统给位置的地方很少：只有 [codex.toml](../../crates/plugins/agents/codex.toml) 的 `[locate.bundled]` 有 `macos` 键（`/Applications/Codex.app/Contents/Resources/codex`）。[claude.toml](../../crates/plugins/agents/claude.toml) 有一条 `[[install.methods]]` 的 `os = ["macos", "linux"]`（`curl … install.sh | bash`），但 `[locate]` 里没有 macOS 的路径；[cursor.toml](../../crates/plugins/agents/cursor.toml) 的安装方法只有 Windows。不分系统的 `[locate].paths` 也只有 `~/.local/bin/…`（claude.toml、cursor.toml），没有 Homebrew 的目录，也没有 `/usr/local/bin`——所以 [DESIGN.md](../DESIGN.md) §9.3「hub 的 PATH 是旧的」那段末尾的「macOS / Linux……`[locate].paths` 已覆盖常见位置」对 macOS 不成立，这一轮先记在这里，不改那段。要补 `/opt/homebrew/bin`（Apple Silicon）和 `/usr/local/bin`（Intel）这类位置，按 `[locate.paths]` 的 `windows` / `macos` / `linux` 键分开写，写法见 [DESIGN.md](../DESIGN.md) §9.3。

**代理、Cursor、进程监督**：

- macOS 的系统代理没读（[N3.md](N3.md) §4 记的是同一个缺口）。Mac 上通常已有 `HTTPS_PROXY`，所以优先级低。
- Cursor 的 `--sandbox` 在 macOS 上会真的启用，行为没测过（[cursor-cli.md](../research/cursor-cli.md)：Windows 上直接报 "Sandbox requires macOS or Linux"）。macOS 上隔离登录要改 `HOME`（它的 `auth.json` 路径写死 `~/.cursor`，不跟 `CURSOR_DATA_DIR` 走），代价是随它跑的 git / ssh / npm 也会看到 profile 里的文件。
- 进程监督的 Unix 实现（进程组代替 Job Object）与 [LOCKS.md](LOCKS.md) §5 的 Unix 实验都**没在 Mac 上跑过**，验收前不能当已实现。

## 5. 建议顺序

四步按原话的顺序拆成阶段，命名用 Mac1–Mac4：`docs/tasks/` 里已有的 M0–M5 是项目早期的里程碑任务书，W1–W4 是 [EXTENSIONS.md](../EXTENSIONS.md) §9「分期」的扩展 api 2 阶段（另 [W1.md](W1.md) 是轻量 Work），三者都不是这里的意思。**阶段划分和每一步的验收都是建议**，原话只给了顺序，没给验收；验收按原话里的问题写成可检查的动作。

**Mac1 macOS CI 跑测试**（建议）

- 范围：在 macOS runner 上跑 `cargo test --workspace`、`npm --prefix apps/gui test`、`npm --prefix apps/gui run typecheck`；CI 文件在做这一阶段时再加。
- 依赖：无，排在最前。它不需要 Mac 机器，也不需要签名。
- 验收（建议）：三个命令在 macOS 上通过；跑不过的用例逐条记下来——预期会有一批 Unix 专有代码第一次被编译。

**Mac2 GUI 小修 + 不签名的 dmg 手测**（建议）

- 范围：§2 的全部五条，加 §3 的 dmg / zip / `icon.icns` / 通用二进制，不含签名与公证。
- 依赖：Mac1 至少 `typecheck` 与 `test` 绿；dmg 只能在 macOS 上打。
- 验收（建议）：输入框里 ⌘C / ⌘V / ⌘X / ⌘A / ⌘Z 都能用；⌘Q 退出、⌘M 最小化；点 Dock 图标窗口回来（这是 §2 第三条要修的 bug，修完应能验证「藏起来 → 点图标 → 回来」）；菜单栏图标在浅色和深色下都看得清；dmg 装上后首次打开能启动（不签名时走「隐私与安全性」放行）。

**Mac3 随 A2 / B1 做环境、终端、字节走 hub**（建议）

- 范围：§4 的 PATH（用登录 shell 取环境，[MULTI-NODE.md](../MULTI-NODE.md) §7.1）、内置终端加 `-l`、描述文件补 macOS 的 `[locate]` 路径、Unix 宿主与锁实验在 Mac 上实测（[TODO.md](../TODO.md) §一那条推迟到 B1 的）。还有「字节走 hub」：Mac GUI 连远程 hub 时，[extProtocol.cts](../../apps/gui/electron/extProtocol.cts) 的 `extensionRealFile` 读本机 `<home>/extension-versions`、`shell:open-path` 打开本机绝对路径，这两处同盘假设都要改成字节经 hub 取，做法见 [EXTENSIONS.md](../EXTENSIONS.md) §9「查看器」与 [MULTI-NODE.md](../MULTI-NODE.md) §16「扩展在多节点下」。
- 依赖：[MULTI-NODE.md](../MULTI-NODE.md) §12 的 A2（远程 GUI：Mac GUI 连 Windows hub）与 B1（node 代理：Windows hub 在 Mac 节点上拉起 Codex）；[MULTI-NODE.md](../MULTI-NODE.md) §15 单干模式定下「本机 hub 加一个内置节点」（出处是 MULTI-NODE §12 B1 那句「单机模式就是 hub 加一个内置节点」）之后，PATH 只需在节点启动时取一次。浏览器视图那条（[EXTENSIONS.md](../EXTENSIONS.md) §9「浏览器视图」）决定摄像头 / 麦克风要不要进 `Info.plist`，也挂在这里。
- 验收（建议）：从 Dock 启动的 GUI 起的 hub 能找到 Homebrew 和 npm 装的 CLI（`/v1/plugins` 里 `installed` 为真、有版本）；内置终端里 `brew` 可用；agent 的会话里 `cargo` / `npm` 可用；在 Mac 上跑一轮真实 agent，宿主用进程组管住整棵树；Mac GUI 连远程 hub 时能打开扩展的静态资源与工作区文件。

**Mac4 签名、公证、自动更新**（建议）

- 范围：§3 的签名与公证一半：Developer ID、hardened runtime、`bin/` 里的 hub 一起签、`electron-updater` 走通。
- 依赖：Mac2（能打出 dmg）与一个 Apple Developer 账号；没有账号这一步做不了（见 §6）。
- 验收（建议）：首次打开不再需要去「隐私与安全性」放行；装 N 版后发布 N+1，GUI 自动下载并在退出时安装。

## 6. 待定

- 手边有没有 Mac 可测。Mac2 起都需要真机或 runner。
- 有没有 Apple Developer 账号（Mac4 的硬依赖）。没有的话只能停在「不签名的 dmg + 手动放行」，也就没有自动更新。
- macOS CI 的费用。私有仓库的 macOS runner 要计费，Mac1 跑一次 `cargo test --workspace` 不便宜；要不要只在 `main` 和发版前跑，等有账号时再定。
