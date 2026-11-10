# macOS 任务书：让项目在 Mac 上跑起来

> 来源：2026-11-07 讨论（run 4c111086）· 相关设计稿：[MULTI-NODE.md](../MULTI-NODE.md) §7.1、§12、§15 · [EXTENSIONS.md](../EXTENSIONS.md) §9「浏览器视图」· 状态：Mac2 的 GUI 代码、Mac3 的终端 `-l` 与 `[locate.paths]` 已改（2026-11-08，run 8298b08a）；不带 hub 的 ad-hoc 包配置已做（2026-11-08，run e28910b3，`c529e55`、`8e90dac`、`6e4392a`）。这些都要在 Mac 上实测。Developer ID、公证、自动更新暂不做（人有 Mac、没有 Apple Developer 账号）。CI、登录 shell 取 PATH 未做 · §7 对照 Cutex 提交 `40ffe16`（2026-11-08）· 在 Mac 上打包与安装见 §8

## 1. 现状

项目从没在 macOS 上跑过。三件事都缺：装得起来、界面上的 Mac 习惯、跑得对。

- **打包**：Windows 仍是 [apps/gui/package.json](../../apps/gui/package.json) 的 `electron:build` / `electron:release`（`--win`，target `nsis`）。hub 的 `crossagents.exe` 和 `icon.ico` 在 `win.extraResources`（2026-11-08，`c529e55`，从顶层 `extraResources` 挪过去）。同一提交配了 Mac 的 ad-hoc、不带 hub 的 dmg / zip，还没在 Mac 上打出来，步骤见 §8。[U1.md](U1.md) §4 记的「打包只做了 Windows」是这之前的状态。
- **没有 CI**：仓库没有 `.github/`，macOS 上的代码一次都没被编译、测试过。这是最便宜、也最先该补的一环——它不依赖有没有 Mac 机器。
- **Unix 代码是真实现，但没跑过**：宿主在非 Windows 上用进程组代替 Job Object，`ps` 查占用、Unix 版测试脚本都写了（[PROCESS-SUPERVISION.md](../PROCESS-SUPERVISION.md) §7）。[TODO.md](../TODO.md) §一「Linux / macOS 上实测宿主」推迟到 B1，[LOCKS.md](LOCKS.md) §5 的 Unix 实验（`flock` 继承、`setsid` 脱离、进程组前任检查）**一次都没跑过**，现在只能按弱保证措辞。
- **已经为 macOS 准备好的**（对照代码核过）：`main.cts` 的 `titleBarStyle: "hiddenInset"` 与 `trafficLightPosition`；`styles.css` 的 `.titlebar.mac`（`padding-left: 80px`，`src/titlebar.tsx` 按 `navigator.userAgent` 加这个类）；`window-all-closed` 区分 `darwin`；[openCopy.cts](../../apps/gui/electron/openCopy.cts) 的 `runsWhenOpened` 已含 `.app` / `.command` / `.sh`；部分快捷键已认 `metaKey`（[App.tsx](../../apps/gui/src/App.tsx) 的 `mod`、[SessionComposer.tsx](../../apps/gui/src/views/SessionComposer.tsx) 的 `composerAction`、[Sidebar.tsx](../../apps/gui/src/Sidebar.tsx) 的旁开）；`main.cts` 起 hub 时已按平台选 `crossagents` / `crossagents.exe`。[titlebar.tsx](../../apps/gui/src/titlebar.tsx) 始终挂载 `<MenuBar />`；Mac 上 [menubar.tsx](../../apps/gui/src/menubar.tsx) 的 `MenuBar` 不渲染菜单栏（`if (isMac) return …`），只留 ⌘/ 的监听和「键盘快捷键」对话框。

页内菜单栏在 Mac 上不显示，原生应用菜单由 §2 第一条补上（代码已改，要实测）。Windows 上应用菜单仍是 `null`。

## 2. GUI 要改的

每条都写文件和符号，行号会漂移，不写。

**应用菜单**。[apps/gui/electron/main.cts](../../apps/gui/electron/main.cts) 的 `Menu.setApplicationMenu(null)` 上方注释说 "Copy and paste still work."——这句在 Windows 上成立，因为 Chromium 自己处理了这些编辑键。macOS 上不是：⌘C / ⌘V / ⌘X / ⌘A / ⌘Z、⌘Q、⌘M 都由应用菜单提供，菜单为空就失效（Electron 已知行为，**要在 Mac 上确认**）。改法：macOS 用 `Menu.buildFromTemplate` 建一份原生菜单，至少「应用」（`role: "appMenu"`，带退出与隐藏）、「编辑」（`role` 用 `undo` / `redo` / `cut` / `copy` / `paste` / `selectAll`）、「窗口」（`minimize` / `zoom` / `front`）；其它平台保持 `null`。页内菜单栏在 Mac 上已经不显示（见 §1），不需要再动。

本轮已改代码，要实测（2026-11-08，`455741e`）：[platform.cts](../../apps/gui/electron/platform.cts) 的 `applicationMenu` 在 darwin 上给出应用、编辑、窗口三份原生菜单，其他平台仍是 `null`。[main.cts](../../apps/gui/electron/main.cts) 用它调用 `Menu.setApplicationMenu`。

**快捷键**。同一文件的 `before-input-event`：缩放那一段判断 `!input.control`，dev server 那一段（`F12`、`input.control && input.shift && i`、`input.control && r`）也只认 `control`。Mac 上要认 `input.meta`——⌘= / ⌘- / ⌘0 是缩放，⌘⌥I 是开发者工具，⌘R 是重新加载。同时 [menubar.tsx](../../apps/gui/src/menubar.tsx) 里每条 `Entry` 的 `keys` 和 `SHORTCUTS` 表按平台显示：Mac 上菜单栏不渲染，但「键盘快捷键」对话框（`⌘/` 打开，监听认 `metaKey`）经 [shortcuts.ts](../../apps/gui/src/lib/shortcuts.ts) 的 `shortcut` 把 `Ctrl` 显示成 ⌘。

本轮已改代码，要实测（2026-11-08）：主进程快捷键在 `455741e`（`platform.cts` 的 `shortcut`，darwin 认 `meta`，字母键看 `input.code`）；界面显示在 `3c49369`。「在旁边打开」在 `6e4392a` 按平台写：[i18n.ts](../../apps/gui/src/lib/i18n.ts) 的 `besideHint`，Mac 上是「⌘+单击或中键：在旁边打开」，其它平台仍是「Ctrl+单击或中键：在旁边打开」。[Sidebar.tsx](../../apps/gui/src/Sidebar.tsx) 和 [SessionPeek.tsx](../../apps/gui/src/SessionPeek.tsx) 都用 `sidebar.besideHint`。处理代码认 `ctrlKey || metaKey`；Mac 上 Ctrl+单击是右键，提示写的是 ⌘。这句在 Mac 上是否照显示、⌘ 单击是否在旁边打开，要实测。

**点 Dock 图标**。`main.cts` 的 `app.on("activate")` 只在 `BrowserWindow.getAllWindows().length === 0` 时 `createWindow()`。但托盘把窗口藏起来而不是关掉：[tray.cts](../../apps/gui/electron/tray.cts) 的 `keep(win)` 在 `close` 里 `event.preventDefault()` 再 `win.hide()`，窗口一直在。于是 Mac 上点 Dock 图标什么都不发生，窗口回不来。改成调已经有的 `showWindow()`（现在 `second-instance` 在用）——它没有窗口时也会 `createWindow()`，两种情形都对。

本轮已改代码，要实测（2026-11-08，`455741e`）：`app.on("activate")` 调用 `showWindow()`。

**托盘**。`main.cts` 里 `createTray(join(process.resourcesPath, "icon.ico"), …)` 写死 `.ico`；macOS 菜单栏要模板 PNG——黑色图形、透明背景，系统按深浅色反色——`nativeImage` 上设 `setTemplateImage(true)` 或文件名以 `Template` 结尾。`tray.cts` 的 `displayBalloon` 和 `balloon-click` 只在 Windows 有效，Mac 上要改用系统通知（`Notification`）。`hintOnce` 的文案「点任务栏右下角的图标可以重新打开」按平台改（Mac 是菜单栏图标）。`Tray` 的 `click` / `right-click` 在 Mac 上的语义也要一并核对（**要在 Mac 上确认**）。

本轮已改代码，要实测（2026-11-08，`455741e`）：托盘图标用 [iconTemplate.png](../../apps/gui/build/iconTemplate.png) 与 [iconTemplate@2x.png](../../apps/gui/build/iconTemplate@2x.png)，darwin 上 `setTemplateImage(true)`；[main.cts](../../apps/gui/electron/main.cts) 从 `process.resourcesPath` 取 `trayIcon` 返回的文件名。提示改用 `Notification`（模块级 `Set` 拿住，避免被回收后点不了）；`hintOnce` 在 Mac 上说菜单栏。单击 / 右键、点通知、浅色 / 深色 / Retina 仍要在 Mac 上确认。打包这一条本轮做了配置、还要实测（2026-11-08，`c529e55`）：顶层 `build.extraResources` 已去掉，Windows 的 hub 与 `icon.ico` 在 `win.extraResources`，`mac.extraResources` 复制 [iconTemplate.png](../../apps/gui/build/iconTemplate.png) 与 [iconTemplate@2x.png](../../apps/gui/build/iconTemplate@2x.png)。打出来的 Mac 包托盘有没有图标，要在 Mac 上确认。步骤见 §8。

**打开即执行的文件**。`main.cts` 的打开判断补 macOS 的 `.terminal`、`.fileloc`——这两个在 Mac 上会被系统当成可执行/定位文件打开，和 `.app` / `.command` / `.sh` 是一类。

本轮已改代码，要实测（2026-11-08，`aeb3e8d`）：判断在 [openCopy.cts](../../apps/gui/electron/openCopy.cts) 的 `runsWhenOpened`，清洗掉末尾的点和空格后再看扩展名。本机模式查绝对路径，远程模式查工作区路径和副本文件名。

**全屏时收进托盘**。[apps/gui/electron/tray.cts](../../apps/gui/electron/tray.cts) 的 `keep` 在 `close` 里 `event.preventDefault()` 之后直接 `win.hide()`。窗口在原生全屏时这样藏，会留下黑屏的 Space，要先退出全屏再藏。Cutex 的做法见 §7：`DesktopShell.hideWindow` 在 `isFullScreen()` 时先 `setFullScreen(false)`，等到 `leave-full-screen` 再 `hide()`。

本轮已改代码，要实测（2026-11-08，`455741e`）：[tray.cts](../../apps/gui/electron/tray.cts) 的 `keep` 在 darwin 全屏时先 `setFullScreen(false)`，收到 `leave-full-screen` 再 `hide()`。**要在 Mac 上确认**没有留下黑屏的 Space。

## 3. 打包与分发

**electron-builder 的 mac 目标**（2026-11-08 已配置，`c529e55`；在 Mac 上打出包并装上，要实测，步骤见 §8）：`dmg` 与 `zip`，各 `arm64`、`x64`，不做通用二进制。图标用已有的 `build/icon.png`，仓库里不另放 `icon.icns`（electron-builder 在 Mac 上从这张 png 转）。包里不带 hub，所以没有按 `aarch64-apple-darwin` / `x86_64-apple-darwin` 编的 hub，也不做 `lipo`。顶层不再有 `extraResources`：`crossagents.exe` 与 `icon.ico` 只在 `win.extraResources`；`mac.extraResources` 只放托盘的 `iconTemplate.png` 与 `iconTemplate@2x.png`。`electron:build` / `electron:release` 仍是 `--win`。`zip` 留给以后的自动更新；本轮 Mac 上不自动更新，见下面和 §8。

**摄像头 / 麦克风**：浏览器视图（[EXTENSIONS.md](../EXTENSIONS.md) §9「浏览器视图」）要开摄像头或麦克风时，macOS 必须在 `Info.plist` 里声明 `NSCameraUsageDescription` / `NSMicrophoneUsageDescription`，并带对应的 entitlement；不声明会出问题，具体表现要在 Mac 上确认。主窗口的权限仍由 `permissions.cts` 的 `permissionAllowed` 决定（只放行 `clipboard-sanitized-write`），这条不变。

**签名与公证**：Developer ID、hardened runtime、公证是 Mac4。2026-11-08：人有 Mac、没有 Apple Developer 账号，暂不做。本轮的包不带 hub，没有要一起签的 hub 二进制。

本轮做的是 ad-hoc 签名：[apps/gui/package.json](../../apps/gui/package.json) 的 `build.mac.identity` 为 `"-"`，`hardenedRuntime` 为 false，`notarize` 为 false（`c529e55`）。`identity` 用 `"-"`，不用 `null`：`null` 会跳过签名，Apple Silicon 上打不开。这不是 Developer ID，也不公证。electron-builder 是 26.15.3。打出来的 `CrossAgents.app` 上 `codesign -dv --verbose=2` 应看到 `Signature=adhoc`，`codesign --verify --deep --strict` 应通过。这两条要在 Mac 上实测，见 §8。从别处拷来的包，首次打开仍可能要去「系统设置 → 隐私与安全性」放行。

**只能在 macOS 上做**：签名和公证都只能在 macOS 上跑，所以要么有一台 Mac，要么用 macOS 的 CI runner；私有仓库的 runner 要计费（见 §6）。Developer ID 与公证是 §5 里 Mac4 的硬依赖，暂不做。本轮的 ad-hoc 包同样只能在 macOS 上打，这台 Windows 打不出来，步骤见 §8。

## 4. 节点与 hub 在 macOS 上

**PATH 是最大的坑**。从 Finder、Dock 或 launchd 启动的进程只有 `/usr/bin:/bin:/usr/sbin:/sbin`；GUI 起 hub 时原样继承——[localHub.cts](../../apps/gui/electron/localHub.cts) 的 `run` 用 `execFile(exe, args, { timeout, windowsHide: true })`，不传 `env`，所以 `process.env` 整个传下去，包括那个残缺的 `PATH`。后果是 npm / Homebrew 装的 CLI 找不到、agent 要用的 `cargo` / `npm` 找不到、扩展工具要的 `node` 找不到。Windows 上靠读注册表绕开（[DESIGN.md](../DESIGN.md) §9.3「hub 的 PATH 是旧的」那段：合并 `HKCU\Environment` 与系统 `PATH`）；macOS 没有注册表，要用登录 shell 取环境——做法见 [MULTI-NODE.md](../MULTI-NODE.md) §7.1（节点启动时用登录 shell 取环境），单机时同样适用（[MULTI-NODE.md](../MULTI-NODE.md) §15 单干模式把本机 hub 也当成一个内置节点，出处是 MULTI-NODE §12 B1 那句「单机模式就是 hub 加一个内置节点」）。

**内置终端的 shell**。[crates/hub/src/terminal.rs](../../crates/hub/src/terminal.rs) 的 `default_shell`（非 Windows 分支）用 `$SHELL`，没设时 `/bin/sh`。

本轮已改代码，要实测（2026-11-08，`c5355fd`）：`unix_shell` 在 `os == "macos"` 时追加 `-l`，Linux 不加。macOS 上内置终端应读到 `~/.zprofile` 里的 Homebrew 路径；还没在 Mac 上跑过。登录 shell 取整份环境（给 hub 的 PATH）仍随 B1，本轮没做。

**CLI 查找**。[crates/plugins/agents/](../../crates/plugins/agents/) 下的描述文件按系统给位置。[codex.toml](../../crates/plugins/agents/codex.toml) 的 `[locate.bundled]` 仍有 `macos` 键（`/Applications/Codex.app/Contents/Resources/codex`）。[claude.toml](../../crates/plugins/agents/claude.toml) 有一条 `[[install.methods]]` 的 `os = ["macos", "linux"]`（`curl … install.sh | bash`）。

本轮已改代码，要实测（2026-11-08，`c5355fd`）：`[locate.paths]` 的 `macos` 键补了 Homebrew 默认前缀。claude、cursor、with 含 `~/.local/bin` 或自家安装目录，再加上 `/opt/homebrew/bin/<命令>`（Apple Silicon）和 `/usr/local/bin/<命令>`（Intel）；codex、codebuddy 是这两条 Homebrew 路径。Windows / Linux 键不含这些路径。Finder 或 Dock 启动的 hub 仍只有最小 PATH，这些路径是那时的退路；hub 启动时用登录 shell 取环境仍随 B1。[DESIGN.md](../DESIGN.md) §9.3「hub 的 PATH 是旧的」已改成与此一致。还没在 Mac 上确认找得到这些 CLI。

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

**Mac2 GUI 小修 + 不带 hub 的 ad-hoc 包手测**（建议）

- 范围：§2 的各条（代码已改，要实测，见 §2）。不带 hub 的 ad-hoc 包配置已做，要实测（2026-11-08，`c529e55` 打包、`8e90dac` 主进程、`6e4392a` 连接页）：dmg / zip，各 arm64 与 x64，图标用 `icon.png`，托盘 png 在 `mac.extraResources`，见 §8。不含 Developer ID 与公证（Mac4，暂不做）。
- 依赖：Mac1 至少 `typecheck` 与 `test` 绿；dmg 只能在 macOS 上打。这台是 Windows，打不出 mac 包。
- 验收（建议）：输入框里 ⌘C / ⌘V / ⌘X / ⌘A / ⌘Z 都能用；⌘Q 退出、⌘M 最小化；点 Dock 图标窗口回来（这是 §2 第三条要修的 bug，修完应能验证「藏起来 → 点图标 → 回来」）；菜单栏图标在浅色和深色下都看得清；按 §8 装上后首次打开能启动，`codesign --verify --deep --strict` 通过，再按 [MULTI-NODE.md](../MULTI-NODE.md) §14「怎么实测」连 Windows hub。

**Mac3 随 A2 / B1 做环境、终端、字节走 hub**（建议）

- 范围：§4 的 PATH（用登录 shell 取环境，[MULTI-NODE.md](../MULTI-NODE.md) §7.1；本轮未做，仍随 B1）、内置终端加 `-l` 与描述文件补 macOS 的 `[locate.paths]`（本轮已改代码，要实测，见 §4）、Unix 宿主与锁实验在 Mac 上实测（[TODO.md](../TODO.md) §一那条推迟到 B1 的）。「字节走 hub」的接口已做（[EXTENSIONS.md](../EXTENSIONS.md) §9「查看器」、[MULTI-NODE.md](../MULTI-NODE.md) §16）：`serveExtension` 向 hub 要扩展字节，远程用默认程序打开的是副本。Mac GUI 连上远程 hub 之后要实测这两处打得开。
- 依赖：[MULTI-NODE.md](../MULTI-NODE.md) §12 的 A2（远程 GUI 的代码已完成，Mac 上连 Windows hub 仍要实测）与 B1（node 代理：Windows hub 在 Mac 节点上拉起 Codex）；[MULTI-NODE.md](../MULTI-NODE.md) §15 单干模式定下「本机 hub 加一个内置节点」（出处是 MULTI-NODE §12 B1 那句「单机模式就是 hub 加一个内置节点」）之后，PATH 只需在节点启动时取一次。浏览器视图那条（[EXTENSIONS.md](../EXTENSIONS.md) §9「浏览器视图」）决定摄像头 / 麦克风要不要进 `Info.plist`，也挂在这里。
- 验收（建议）：从 Dock 启动的 GUI 起的 hub 能找到 Homebrew 和 npm 装的 CLI（`/v1/plugins` 里 `installed` 为真、有版本）；内置终端里 `brew` 可用；agent 的会话里 `cargo` / `npm` 可用；在 Mac 上跑一轮真实 agent，宿主用进程组管住整棵树；Mac GUI 连远程 hub 时能打开扩展的静态资源与工作区文件。

**Mac4 签名、公证、自动更新**（建议）

- 2026-11-08：人有 Mac、没有 Apple Developer 账号，暂不做。
- 范围：§3 的 Developer ID、hardened runtime、公证，以及 `electron-updater` 走通。本轮的包不带 hub，没有要一起签的 hub 二进制。
- 依赖：Mac2 的包能在 Mac 上装起来，以及一个 Apple Developer 账号；没有账号这一步做不了（见 §6）。
- 验收（建议）：首次打开不再需要去「隐私与安全性」放行；装 N 版后发布 N+1，GUI 自动下载并在退出时安装。

## 6. 待定

- 手边有 Mac（2026-11-08 定）。Mac2 起的打包和界面要在这台 Mac 上实测。这台开发机是 Windows，mac 包在这里打不出来，agent 也不能在那台 Mac 上跑命令。
- 没有 Apple Developer 账号（2026-11-08 定，Mac4 的硬依赖）。Mac4 暂不做。现在停在 ad-hoc 签名、不公证、不自动更新，见 §8。
- macOS CI 的费用。仓库 L1nkkkk/CrossAgents 是私有的（2026-11-08 核实），macOS runner 要计费。Mac1 跑一次 `cargo test --workspace` 不便宜；要不要只在 `main` 和发版前跑，等有账号时再定。Cutex 的公开仓库不计费，见 §7。

## 7. Cutex 的做法

> 对照 Cutex desktop v0.2.2，提交 `40ffe16003e27b29d890e9fe1ba9f309dd379e7d`。2026-11-08 读了这一提交的源码和安装文档，没有运行。下面只写核过的事实。

结论：Mac 版只有前端，经 SSH 连一台 Linux 或 Windows 主机。打包不用 electron-builder；内容放齐后整包 ad-hoc 签名，再跑 `codesign --verify --deep --strict`。不公证，不出 dmg，按架构各出一个 zip。

- **只有前端，经 SSH 连主机。** [docs/install/AGENT-INSTALL.md](https://github.com/senxiu-official/cutex-desktop/blob/40ffe16003e27b29d890e9fe1ba9f309dd379e7d/docs/install/AGENT-INSTALL.md) 开头的平台表写 macOS 是 “the desktop only”：agent 运行时不支持 macOS，要 SSH 到自己的 Linux 或 Windows 机器（§7.4）。§7.4 让人填 `user@machine`，用已有的 SSH 密钥和 `~/.ssh/config` 做隧道并配对，不用在主机上开业务端口。配对时跑的命令是 `cutex management client-token issue`。
- **不用 electron-builder。** [scripts/package-portable.mjs](https://github.com/senxiu-official/cutex-desktop/blob/40ffe16003e27b29d890e9fe1ba9f309dd379e7d/scripts/package-portable.mjs) 自己组包。`stampAppBundle` 把 `Electron.app` 改名为 `Cutex.app`，用 PlistBuddy 改 Info.plist，把可执行文件从 `Electron` 改名为 `Cutex`，这样 `app.isPackaged` 为真。`buildIcns` 把已有尺寸的 png 放进 iconset，再 `iconutil -c icns`（函数注释写 sips + iconutil，这一提交的函数体没有调用 sips）。内容放齐之后才签名：`codesign --force --deep --sign -`（ad-hoc；环境变量 `CUTEX_DESKTOP_CODESIGN_IDENTITY` 有值就用那张证书），然后 `codesign --verify --deep --strict`。`stampAppBundle` 的注释写明这不是 Developer ID，也不公证。
- **按架构各出一个 zip，用 `ditto` 打包。** [.github/ci/package-desktop.sh](https://github.com/senxiu-official/cutex-desktop/blob/40ffe16003e27b29d890e9fe1ba9f309dd379e7d/.github/ci/package-desktop.sh) 对 `macos-*` 先再跑一遍 `codesign --verify --deep --strict`，再用 `ditto -c -k --sequesterRsrc --keepParent` 打 zip。脚本注释写 `zip -r` 会破坏符号链接、权限和签名。arm64 与 x64 各一个包，不做通用二进制。x64 是在 arm64 的 Mac 上换 x64 的 Electron 运行时再打包（脚本注释：应用本身只有 JavaScript）。没有 dmg 目标。
- **CI 用 macos-15，公开仓库不计费。** [.github/workflows/build-binaries.yml](https://github.com/senxiu-official/cutex-desktop/blob/40ffe16003e27b29d890e9fe1ba9f309dd379e7d/.github/workflows/build-binaries.yml) 的 matrix 里 `macos-arm64` 和 `macos-x64` 都是 `runner: macos-15`。仓库公开（未登录就能读到这次提交）。公开仓库的 GitHub-hosted macOS runner 不计费。我们的仓库要计费，见 §6。
- **ad-hoc 签名下，safeStorage 每次更新都重新弹钥匙串。** 同一份 `package-portable.mjs` 开头写：ad-hoc 下每个包都是新身份，钥匙串里的 “Cutex Safe Storage” 每次更新都会再问；设 `CUTEX_DESKTOP_CODESIGN_IDENTITY` 用自签的代码签名证书，可以让 Always Allow 跨更新保留。安装文档 §10 写主机 token 加密在钥匙串里，ad-hoc 包每次更新都算一个新程序。[apps/desktop/src/main/shell.ts](https://github.com/senxiu-official/cutex-desktop/blob/40ffe16003e27b29d890e9fe1ba9f309dd379e7d/apps/desktop/src/main/shell.ts) 的 `DesktopShell` 用 `safeStorage` 存更新代理的口令（`setUpdateProxy`、`updateProxyConfig`）。主机 token 的存放点不在这个文件里，以安装文档 §10 为准。
- **原生全屏时先退出再藏。** 同一个 `DesktopShell.hideWindow`：`isFullScreen()` 为真时先 `setFullScreen(false)`，收到 `leave-full-screen` 再 `hide()`。注释写直接藏会留下黑屏的 Space。我们这边见 §2「全屏时收进托盘」。

## 8. 在 Mac 上打包与安装（不带 hub、ad-hoc 签名）

照着装的步骤（含连 Windows 上的 hub）整理在 [INSTALL-MAC.md](../INSTALL-MAC.md)；这一节记配置依据和还没实测的项。

2026-11-08（run e28910b3）配好了，还没在 Mac 上打出来、装上去。这台是 Windows，electron-builder 只在 macOS 上打 mac 包。下面每条标了「要实测」的，都是还没在那台 Mac 上跑过的。Windows 的安装包仍走 `electron:build` / `electron:release`，这一节不改它。

**前提。** macOS 上安装 Node 与 git。这台机器没有核对 Mac 上要哪一个版本，要实测。要 Rust（2026-11-10 起）：`electron:build:mac` 先 `cargo build --release -p crossagents-link`，包里带打洞程序（[MULTI-NODE.md](../MULTI-NODE.md) §14「内置打洞」）；[check-version.mjs](../../apps/gui/scripts/check-version.mjs) `--without-hub` 检查它在且版本对，不检查 hub 二进制。

**步骤。**

1. `git clone https://github.com/L1nkkkk/CrossAgents.git`，进入仓库。
2. `npm --prefix apps/gui ci`
3. `npm --prefix apps/gui run electron:build:mac`

这条脚本是 `node scripts/check-version.mjs --without-hub`，然后 `npm run build`、`npm run electron:compile`、`electron-builder --mac --publish never`。产物在 `apps/gui/release/`。配置里 dmg 和 zip 各打 arm64 与 x64，文件名是 `CrossAgents-<版本>-<arch>.dmg` 和同名的 `.zip`（[package.json](../../apps/gui/package.json) `build.mac.artifactName`）。装的时候按这台 Mac 的芯片选 arm64 或 x64。另一架构能不能在这台机器上打出来，要实测。

**核对签名。** 装好以后：

- `codesign -dv --verbose=2 /Applications/CrossAgents.app` 应看到 `Signature=adhoc`
- `codesign --verify --deep --strict /Applications/CrossAgents.app` 应通过

还没装时，electron-builder 解出的 app 在 `apps/gui/release/mac-arm64/CrossAgents.app`（arm64）与 `apps/gui/release/mac/CrossAgents.app`（x64）。目录名是 electron-builder 的默认（app-builder-lib/out/platformPackager.js `computeAppOutDir`：mac 加架构后缀，默认架构 x64 不加），要实测。

`build.mac.identity` 是 `"-"`（ad-hoc），`hardenedRuntime` 是 false，`notarize` 是 false。要实测。

**首次打开。** 在这台 Mac 上自己打的包没有隔离标记，一般不会被拦（要实测）。从别处拷来的（浏览器下载、AirDrop、聊天软件）会被拦。这时走 系统设置 → 隐私与安全性 →「仍要打开」，或 `xattr -dr com.apple.quarantine /Applications/CrossAgents.app`。

**不带 hub。** 包里没有 hub，也不连这台 Mac 上另外运行的 hub，只能连另一台电脑上的 hub。说的是打出来的安装包。`npm --prefix apps/gui run electron:dev` 不走这条，仍连本机正在跑的 hub。

打开后侧栏的离线原因是「未连接：」接上 [main.cts](../../apps/gui/electron/main.cts) 的 `NO_BUNDLED_HUB` 原文：「这个安装包不带 hub：在 设置 →「连接」 里连另一台电脑上的 hub」。侧栏标明现在连哪的那一行是「本机 · 不带 hub」，悬停也是这句话（[connection.ts](../../apps/gui/src/lib/connection.ts) `markWords`）。

设置 →「连接」（[Connection.tsx](../../apps/gui/src/views/Connection.tsx)）：本机且包里没有 hub 时，「现在连的是」下面写「这个安装包不带 hub，只能连接另一台电脑上的 hub」，状态芯片是「不带 hub」。已经配对过一台中心时，「切回本机」用同一句说明，按钮不能点（`bundledHub` 为 false）。这些文案按代码写的，打开后的样子要在 Mac 上确认。

**更新。** Mac 版不能自动更新：没有 Apple 签名，Squirrel.Mac 不接受 ad-hoc 签名的包，发布页上也没有 Mac 包。要更新就在 Mac 上拉新代码、重新打包，再替换「应用程序」里的 CrossAgents。

设置里的更新卡片显示 [updates.cts](../../apps/gui/electron/updates.cts) `startUpdates` 的原文：「Mac 版没有 Apple 签名，不能自动更新：要更新就重新打包，再替换「应用程序」里的 CrossAgents。」这一状态下没有「检查更新」按钮，也不写「每 4 小时自动检查一次」。托盘菜单里没有检查更新那一项（[tray.cts](../../apps/gui/electron/tray.cts)，`manual` 时不放入）。卡片和托盘要在 Mac 上确认。

**钥匙串。** 连接用的 token 由 Electron `safeStorage` 加密，密钥存在钥匙串里，密文写在 `<userData>/remote.json`（[remote.cts](../../apps/gui/electron/remote.cts) `sealToken` / `openToken`）。重新打包后签名会变，打开时钥匙串可能再问一次，选「始终允许」（要实测）。拒绝的话保存的配对读不出来：`openToken` 解不开就返回空，[remoteController.cts](../../apps/gui/electron/remoteController.cts) `connect` 把隧道标成离线，原因是 `REPAIR`（「请重新配对」），并且不再自动重连。连接页上就是「未连接」，说明是「请重新配对；不会再自动重连」（要实测）。

**接着连 Windows 上的 hub。** 按 [MULTI-NODE.md](../MULTI-NODE.md) §14「怎么实测」。Mac 上用的就是这一节打出来的包。
