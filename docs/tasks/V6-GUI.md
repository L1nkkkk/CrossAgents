# V6 任务书：让界面经得起看，并把 Cutex 剩下的借完

> 前序：[V5](V5-GUI.md) · 依据：[Cutex 前端拆解](../research/cutex-gui-teardown.md) §1.5、§4、§6、§7 · 2026-10-22

## 1. 问题

V5 之后界面「不像 AI 模板」了，但还是不好看。在空 hub 上打开 GUI，看到的是一片灰白：侧栏、页面、卡片四个底色挤在 2% 亮度里，
中英文混排不齐，图标粗细不一。更根本的是：**调样式必须起真 hub、建真 agent、跑真 run**，视觉问题在有数据的状态下才看得见，
而这个状态很难反复进入。所以样式一直是「能用」而不是「好看」。

Cutex 拆解里还有几项没借：纯前端 mock 预览、轮尾摘要和按轮 diff、右侧工具列、多套浅色配色、时间显示规则、i18n。V6 把这些做完。

## 2. 范围

| 优先级 | 内容 | 依赖 |
|---|---|---|
| P0 | 前端 mock 预览模式（§3） | 无 |
| P0 | 视觉层次返工：明度、字体、图标、手动主题（§4） | §3 做完再调 |
| P1 | 轮尾摘要与按轮 diff（§5） | hub 发布驱动的 `TurnDiff`（N1 §6） |
| P1 | 右侧工具列：改动、任务、事件流三个窗格（§6） | §5 |
| P2 | 时间显示规则、i18n 骨架（§7） | 无 |

不做：插话（steer）、停止后暂停队列、队列拖动排序，都要驱动在工具边界读入，留给驱动任务书。多主机、设备权限、内置浏览器、插件沙箱仍按拆解 §8 以后再议。

## 3. mock 预览模式（P0）

目标：`npm run preview:mock` 在普通浏览器里打开 GUI，不需要 hub，数据来自前端自己的假数据，可以一键切换到几种典型场景。

- 切入点是 `api.ts` 的 `bridge` 和 `hub.ts` 的 SSE 读取：加一个 `MockBridge`，实现和 `window.crossagents` 相同的接口（`bridge.d.ts`），
  `fetch("/v1/...")` 由它应答，`/v1/events` 由它按脚本推事件。`VITE_MOCK=1` 时启用，生产包不含 mock 代码（动态 `import()` + 构建期 tree-shake）。
- 假数据按模块放在各自目录的 `mock.ts`：`views/Home/mock.ts`、`views/Session/mock.ts`、`views/Run/mock.ts`、`views/Agents/mock.ts`、`views/Usage/mock.ts`。
  共用的实体（团队、工作区）放 `dev/fixtures.ts`。
- 场景切换：页面右下角一个只在 mock 模式出现的小面板，列出场景，切换即重置状态：
  - 空 hub（现在看到的）
  - 一个 Planner + 两个 Worker 的 run：计划待审批
  - 同一个 run：两个任务运行中、一个待合并、一个失败，Planner 对话里有编排消息行和任务卡片，侧栏有「需要你」
  - 单聊：Claude 的会话，有折叠的工具步骤、一轮进行中、排队两条消息
  - 用量页：两个账号、有限额告警
  - 窄窗口（900px）下的以上任一场景
- 事件脚本：mock 的 `/v1/events` 支持「回放」一段事件序列（带相对时间），用来看进行中的呼吸点、本轮状态行、工具步骤逐条出现。脚本和 fixtures 一起放 `dev/`。
- 验收：`npm run preview:mock` 不起 hub 能打开所有场景；`vite build` 产物里 grep 不到 `MockBridge`。

## 4. 视觉层次返工（P0）

全部在 §3 的场景里调，每改一项截前后对比图。

### 4.1 明度与层次

- 浅色：`--page` 压暗到 `#f4f4f2` 一档，`--surface` 保持纯白，让卡片、输入区、停靠面板真正浮在页面上；`--sidebar` 介于两者之间。
  深色：窗口底色 `--page` 比对话区更暗（Cutex 是 `#0e0e0f` 对 `#161616`），对话区和侧栏用 `--surface` 加 6% 白边浮起。
- 边框分两档：`--border` 用于分隔，`--border-strong` 只用于输入框和焦点；卡片不同时有边和阴影，浮层（菜单、停靠面板）才有阴影。
- 状态色拆成 fill 和 text 两个 token（`--ok-bg` / `--ok` 已有，补 `--warn-text`、`--ok-text`、`--danger-text`），保证小字对比度 ≥ 4.5:1。
- 把现在 13 处 `box-shadow` 过一遍，只留浮层和 `.dot.off` 的描边。

### 4.2 字体

- `--font-sans` 首选改为 `Inter`（随包带 woff2，`font-display: swap`），回落 `Segoe UI`、`PingFang SC`、`Microsoft YaHei UI`。
  原因：`Segoe UI Variable Text` 没有中文字形，中文回落到雅黑后字重和基线对不上。
- 中文回落单独调：`font-size-adjust` 或对 `:lang(zh)` 减 0.5px，让「团队」和 `Planner` 并排时高度一致。
- 等宽：`JetBrains Mono` 优先于 `Cascadia Code`，同样随包带；工具步骤行、路径、diff 统一 12.5px。
- 数字继续 `tabular-nums`；标题 600 字重、不放大（V5 已定，不变）。

### 4.3 图标

- 引入 `lucide-react`，替换 `components.tsx`、`Sidebar.tsx`、`strips.tsx`、`Home.tsx`、`Session.tsx` 里手写的 svg；统一 16px、1.75 线宽。
- 保留自己画的三样：品牌标、Claude 星芒、Codex `>_` 方块。
- 状态符号 ○ ● ✓ ✕ ◌ 仍然是字符，不换成图标（V4 §2 的状态表不变）。

### 4.4 主题

- 设置里加主题：跟随系统 / 浅色 / 深色；浅色再分 素白、纸（`#faf9f5` 暖）、雾（`#f3f4f6` 冷）。存 `localStorage` 的 `crossagents.theme`。
- 实现：`<html data-theme="light-paper">`，`styles.css` 按 `data-theme` 覆盖 token，`prefers-color-scheme` 只在「跟随系统」时起作用。
- Electron 的 `titleBarOverlay` 颜色跟着主题改（主进程监听渲染进程的主题变更）。

### 4.5 空状态

- 首页空状态不再只有一行灰字：显示三步引导（添加队友 → 选 Planner → 发起 run），每步可点，完成的打勾。
- 侧栏空状态同理，一行文字加一个「添加队友」按钮。

## 5. 轮尾摘要与按轮 diff（P1）

hub 侧：

- 驱动已有 `TurnDiff` 事件（N1 §2）。hub 把它存成 `turn_diffs(agent_id, turn_id, files[], added, removed, snapshot_at)`，
  发 `agent.turn_diff` 事件，`TranscriptTurn` 加 `diff?: { files: FileChange[]; added: number; removed: number }`。
- `GET /v1/agents/{key}/turns/{turn_id}/diff` 返回这一轮的 unified diff（从 worktree 用 `git diff <base>..<head>` 现算，不存全文）。
- 驱动没发 `TurnDiff` 时（能力表里没有），hub 在每轮结束用 `git status --porcelain` 自己算一份 stat，标 `source: "hub"`。

GUI 侧：

- 一轮结束后，回复下面一行淡色小字：`✎ 改了 3 个文件 +12 −4 · src/auth.rs · src/login.tsx · +1 ▸`。只读的轮次不显示。
- 展开后每个文件一行：文件名、`+N −M`、「查看变更」（在右侧工具列打开这一轮的 diff，见 §6）、「复制路径」。
- 任务卡片（V4 §3）的「改动」改为汇总整个任务所有轮次的 diff stat，并能跳到任一轮。
- CLI：`crossagents session diff <key> [--turn N]` 同一份数据。

## 6. 右侧工具列（P1）

- 现在的右侧抽屉（`.app.with-drawer`）升级为工具列：浏览器式标签，每个标签是 **改动**、**文件**、**任务**、**提交图** 之一（调试时还有 **事件流**）。标签可关闭、可排序，可拖到对话下方的面板；不再有上下叠放的窗格。
  放不下时退化为抽屉，两边的标签合成一排，不能拖。
- 窗格内容跟着当前聚焦的对话走，并排两栏时跟着最后点击的那栏；别的会话的内容不会混进来。
- **改动**：§5 的按轮 diff，顶部可以切轮次；diff 用自己的渲染（已有 `lib/diff.ts`），不引第三方。
- **任务**：现有任务详情（规格、总结、验收、改动）原样搬进来。
- **事件流**：这个会话的原始 hub 事件，带过滤，调试用；默认不打开。
- **终端**：也是标签，默认开在对话下方，`Ctrl+\`` 或标签栏的 + 打开。shell 跑在 hub 这台机器上（`crates/hub/src/terminal.rs`），以系统用户身份运行，不受 agent 沙箱限制，输出不进模型。按项目分组：同一项目的会话看到同一组终端，切到别的项目就隐藏，但继续运行。关窗口或断线只是断开，重连时用屏幕快照恢复；只有点 × 才结束，或没人连、也没东西在跑满 2 小时后回收（`[hub] terminal_idle_minutes`），hub 停止时一起结束。默认 shell 可用 `[hub] terminal_shell` 改。
- 布局（每边有哪些标签、显示哪个）记在 `localStorage` 的 `crossagents.panes`（和 V4 的并排布局同一个 key，扩展结构）。

## 7. 时间与 i18n（P2）

- 时间：一小时内显示相对时间（刚刚、12 分钟前），之后显示钟点（11:02、昨天 18:40、10-21 09:15）；悬停显示完整的开始 → 结束。
  写成 `lib/time.ts` 纯函数，有测试；所有显示时间的地方统一调用。
- i18n 骨架：`lib/i18n.ts` 提供 `t(key)`，字典先只有 `zh-CN`，把 `Sidebar.tsx`、`Home.tsx`、`strips.tsx` 里的文案抽出来作为样板，
  其余文件在后续任务里逐步迁移。术语（run、Planner、Worker）不翻译。

## 8. 交付顺序

1. §3 mock 模式，先只做「空 hub」和「run 进行中」两个场景，够 §4 用。
2. §4 视觉返工，逐项截图对比。
3. §3 补齐其余场景。
4. hub 的 `TurnDiff` 落库与 API（§5 hub 侧），可以和 2、3 并行。
5. §5 GUI 侧、§6 工具列。
6. §7。

## 9. 验收

- `cargo test --workspace`、`npm test`、`tsc`、`vite build` 通过；新增 `lib/time.test.ts`，`lib/i18n` 有字典缺 key 的测试。
- `npm run preview:mock` 能打开 §3 列出的全部场景；`vite build` 产物不含 mock 代码。
- 浅色三套、深色一套，各在「空 hub」「run 进行中」「单聊（工具步骤 + diff）」「用量」四个场景截图，和 V5 对比。
- 真 Codex 走一遍 Planner + Worker 的 run，Worker 每轮有轮尾摘要，右侧「改动」窗格能看到每轮 diff，切轮次正确。
- 用 mock 插件（驱动不发 `TurnDiff`）跑一遍，轮尾摘要仍然有 stat，标 `source: "hub"`。
- 窗口 900px 宽时工具列退化为抽屉，并排两栏仍可用。

## 10. 留给下一阶段

- 插话（steer）、停止后暂停队列、队列拖动排序：需要驱动支持在工具边界读入，另开驱动任务书。
- `useRunTree` / `useAgents` 合成一个共享存储（V4 §8 遗留），mock 模式做完后更容易改。
- 终端窗格：PTY 放 hub 进程里、关面板只断开、再开回放（拆解 §4）。
- 多主机、设备权限、内置浏览器、插件沙箱（拆解 §8）。

## 11. 实现记录（2026-10-22）

和本书原文的出入，以这里为准：

- mock 数据按视图放在 `views/*.mock.ts`（views 现在是单文件，不拆目录）；共用实体在 `dev/fixtures.ts`，场景在 `dev/scenarios.ts`，桥在 `dev/MockBridge.ts`。
  `main.tsx` 在 `__MOCK__`（vite `define`，只有 `--mode mock` 为真）分支里动态 import `dev/mock`，其余构建整个分支被删掉；`api.ts` 的 `bridge` 改成 `getBridge()` 惰性读取，好让 mock 先装上。
  窄窗口场景是把应用放进 900px 的 iframe（`?narrow=1`），这样媒体查询看到的是真窄。
- `agent.turn_diff` 加了两个可选字段：`message_id`（所属轮次）和 `source`（`driver` / `hub`）。没有单独建 `turn_diffs` 表：
  diff 全文已经在事件里，transcript 折叠时只算 stat（`TurnDiffStat`），`GET /v1/agents/{key}/turns/{message_id}/diff` 再折叠一遍取全文。
  驱动没有 `structured_diff` 时，hub 在每轮前后各写一个 git tree（用私有 index，不碰 agent 的 index 和 refs，尊重 `.gitignore`），两棵树的 diff 就是这一轮的改动；
  这比 `git status --porcelain` 准：轮前已有的未提交改动不算进这一轮。只读轮次不发事件。
- `TranscriptTurn.diff` 从 diff 文本改成 `TurnDiffStat`（不兼容的类型变化，GUI 已同步）。
- 右侧工具列在会话页由会话顶栏的按钮开关；任务路由（`#/runs/<id>/tasks/<task>`）总在第一个窗格显示任务。并排两栏时窗格跟最后点击的那栏。
- 设置页 `#/settings`（侧栏底部齿轮）放主题。
- i18n 测试会扫 `Sidebar.tsx`、`Home.tsx`、`strips.tsx` 里所有 `t("…")` 的 key，字典缺一个就失败。
- 截图在 `docs/screens/v6/`。真 Codex 的 Planner + Worker 全流程还没有跑（需要账号），留到合并前手动验收。
- `crates/mock-agent/tests/cli.rs` 的 `full_run_with_a_directory_plugin` 在本机 HEAD 上就失败（run 停在 running、任务 passed 后 Planner 不接手），与 V6 无关，另查。
