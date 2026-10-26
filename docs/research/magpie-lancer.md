# magpie 与 Lancer：可借鉴的设计

> 2026-10-26 调研。magpie 读的是 README 与 `docs/reference.md`；Lancer 只看过一段 43 秒的演示视频，像设计原型，后端能力未知。

## 1. 两个产品各是什么

| | magpie（[yetone/magpie](https://github.com/yetone/magpie)） | Lancer（演示视频） |
|---|---|---|
| 层次 | agent 之下：请求走哪家模型、哪个账号 | agent 之上：多条任务线里谁接谁的活 |
| 形态 | 菜单栏 + 本机网关 `127.0.0.1:3425`，四种 API 互译 | 桌面端：左会话列表、中对话、右「工作树 · 泳道」 |
| 与我们的关系 | 互补，可以叠在我们下面用 | 定位几乎相同，交互可直接对照 |

一句话：从 magpie 学额度和账号，从 Lancer 学怎么把多 agent 协作展示出来。

## 2. 建议做的（按优先级）

### P0

1. **交接卡**（Lancer）。交接作为一等对象：发起方 → 接手方、文件、摘要、决策、待解问题、交接时上下文大小。
   在 `task_assign`、改派（`task.rerouted`）、克隆接手时生成，事件里带上这些字段，对话里显示成卡片。
   内容基本就是 §7 任务规格，现在只是没展示。先在 `api-types` 定结构，泳道上的「交接」节点靠它才有内容。
2. **选账号**（magpie）。改 [routing.rs](../../crates/hub/src/routing.rs)：
   - 默认先用最早续期的账号（重置时剩余额度作废）；
   - 两档阈值：新克隆的 Worker 到 90% 就让给别的账号，正在返工的 Worker 到 98% 才换（保住提示缓存）；
   - 失败的账号「休息」排到队尾，不剔除；额度未知的账号先试一次，顺便读出余量。

### P1

3. **Run 泳道图**（Lancer）。一个 Worker（或任务线）一根竖线，节点类型：提交、交接、测试 ✓/✗、重试、改派（分叉）、`land`（合并）、上下文压缩、模型切换、等你。
   数据全在事件流里，主要是前端工作。悬停节点高亮对话里的对应位置。必须有「聚焦」模式，否则 Worker 一多竖线就挤爆。
4. **接力链条**（Lancer）。Run 页顶部每条任务线一行：`Rex ✓ → Ada → 你`，当前在谁、下一棒是谁、哪里在等人一眼看清。
5. **额度快作废提醒发给 Planner**（magpie）。窗口快重置但还剩很多时，告诉 Planner「codex 账号 B 还剩 60%，2 小时后重置」，让它多派活。直接服务 DESIGN §1.1「额度用在对的地方」。
6. **「需要你」内嵌在时间线**（Lancer）。现在已有输入框上方的「需要你」条（`strips.tsx`）；再在事件流和泳道节点上显示同一个待办，A/B 选项可行内点选。
7. **工具步骤按产出命名**（Lancer）。现在已有步骤折叠和 `summarize(steps)`；摘要改成「提交/保存 · keyframes ×6 · 5 步 · 28s」这种按一轮产出命名的写法，产物（图片、文件）在折叠行下直接给缩略图。

### P2

8. **选路可解释**（magpie）。`run trace` 里给出候选账号的排序和理由，不只是最终 `reason`。
9. **设备级额度**（magpie）。多节点 A1 建 `devices` 表时顺手加日 / 周 / 月 token 与成本上限。
10. **CLI 版本**（magpie）。Agents 页显示各 CLI 版本，不在已验证范围时标出（对应 DESIGN §9.4、TODO §五「运行时版本钉住」）。
11. **从现有登录导入账号**（magpie）。读 `~/.codex/auth.json`、`~/.claude/.credentials.json` 导入，仍落到 M5 的独立目录。注意：刷新 ChatGPT 令牌会让原工具被登出，导入前要提示。

## 3. 不学的

- magpie 的意图路由（小模型逐轮选模型）：违反「hub 不思考」，派给谁由 Planner 定。
- magpie 的隐私打码、协议翻译网关：要在请求路径上，我们不在那一层，需要时直接接 magpie。
- Lancer 的「context 8.8k → 8.8k 无损」：跨厂商交接一定会重组上下文，与我们「不做跨厂商迁移」的非目标冲突。卡片写「交接了什么」，不承诺无损。

## 4. 与 magpie 叠用时的约定

Profile 的 `env` 把 `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL` 指向 magpie 即可让 Claude Code 跑 DeepSeek、Kimi 等。
但若指向 magpie 的路由组，它在内部换号，hub 看不到真实额度，两层会同时换号。约定：这类 Profile 视为 hub 不管额度（`accounts.manages` 为 false），magpie 管换号，hub 管派人。

## 5. 印证

magpie 用 Claude 订阅时驱动本机真实 `claude`、用 MCP 桥接工具，原因是 Anthropic 会把别家系统提示判成第三方流量。与 `claude-native` 同一条路；项目规则继续用 `--append-system-prompt` 追加，不整体替换系统提示。
