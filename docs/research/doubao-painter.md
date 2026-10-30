# dsh-doubao-painter 评估

2026-10-29。看的是 [GitHub README](https://github.com/liangzz1998/dsh-doubao-painter) 与本机豆包 2.32.6 的对照。插件本身没装（`dsh` 不在 `PATH`，`D:\dsh-plugins` 不存在，`~/.dsh` 为空），所以下文的"实测"都是对**豆包**的实测，不是对插件的。

## 判断

核心洞察是对的，而且是这类需求里最便宜的解。

同一张图在 CDN 上有两个变体：`…-cthumb_lwm3:…`（384×216，右下角烧了「豆包 AI生成」）和 `…-cgen_wm0_hflow:…`（2848×1600，干净）。聊天流里只挂缩略图，点开预览才把原图 URL 挂进 DOM。所以"去水印"最便宜的解是不去——**在正确的层拿本来就没水印的那张**。比 LaMa/IOPaint（要算力、会毁构图）和裁切都干净。

它拿稳定性换钱：免费额度 + UI 自动化，替掉 API Key + 按张付费。

## 本机能对上的

| README 的说法 | 本机（2.32.6） |
|---|---|
| 豆包桌面端是 Electron（Chromium 147） | 主进程注释 `ver=147.0.7727.149`；有 `agent_infra.mojom.AgentInfraProcess`、renderer、gpu、network 全套 |
| 112 个技能、47 个带脚本 | 107 个技能、46 个带 `scripts/`、679 个 `.py`（差一个小版本） |
| 必须带 `--remote-debugging-port` 启动 | 现在跑着的是普通模式：主进程命令行只有 `--start_time=…`；两个 127.0.0.1 端口都不是 CDP |

**没验证的两条**：`--remote-debugging-port` 在这台机器上是否真的被接受（要验就得退出你正在用的豆包，我没动）；CDN 模板名与 DOM 选择器在 2.32.6 上是否仍然有效。

## 接进 CrossAgents：走 MCP，不要做 driver

`Driver` 是会话型编码 agent 的抽象——`prompt / interrupt / resume / fork / rollback / permission`。生图没有这些语义，硬塞进去会让 `capabilities()` 里大半能力是假的，也污染 `with-native`、`claude-native` 那条线的一致性。

它是**能力**，形状应该是 MCP：

- 机器接口已经有了：`node tools/paint.mjs --stdin-json --json`。
- 外面套一个 stdio MCP server，暴露 `doubao_paint` / `doubao_status`，约 100 行。
- 仓库里已有现成模板：`crates/cli/src/cmd/mcp.rs` 的 `tools/list` / `tools/call`。

**缺口**：CrossAgents 现在只挂一个 MCP server——`crates/hub/src/collab.rs:164` 里硬编码的 `crossagents`，没有"额外 MCP"的配置口。要挂第三个得补一个通用项（`[mcp.<name>] command/args/env`，在 `prepare` 里 append），约 50 行。这是通用能力，任何第三方 MCP 都受益，不只是豆包。

## 代价（README 列了 6 条，补两条它没说的）

1. **独占和并行冲突**：跑图时会开新对话、点开预览，窗口被人和机器抢。CrossAgents 是多 worker 并行编排，生图只能串行，且不能和人同时用豆包。
2. **常驻调试端口 = 常驻后门**：CDP 让本机任何进程都能完全控制那个**已登录**的豆包（读会话、发消息、下文件）。绑 `127.0.0.1` 挡不住本机进程。要用就临时开，别做成开机自启。
3. 豆包一更新就可能静默失效（选择器 + CDN 模板名都是私有实现），必须有 `doubao_status` 健康检查和失败告警，别让 worker 干等 180 秒。
4. 吃免费额度（按天），批量会撞上限。对外发布仍要按《人工智能生成合成内容标识办法》标识——README 自己也提醒了，这点值得肯定。

## 建议：双轨

| 场景 | 用什么 | 理由 |
|---|---|---|
| 正式素材、要并发、要可重复 | 方舟 `seedream` API | 付费但稳定、无水印、可并发、有 SLA |
| 草稿、试错、中文海报 | 这个 painter | 免费、2K、中文文字渲染准 |

CrossAgents 本来就是"能力缺失降级而不报错"，两条并存很自然：默认走 API，painter 作为显式开启的 UI 自动化工具。

## 如果决定接，顺序

1. 先在 DSH 里按 README 跑通一次——那是它的原生宿主，排障最省事，也能顺手验证 CDP 与选择器。
2. 通了再写 `mcp-server.mjs` 包住 `paint.mjs`，在 CrossAgents 加 `[mcp.painter]` 配置 + `prepare` 里 append。
3. `doubao_status` 在端口没开时返回明确指引（"退出豆包，用 `--remote-debugging-port=9225` 重启"），而不是超时。

## 已落地（2026-10-29）

"CrossAgents → 豆包"这条出站链路已经接好，两段都通：

1. **hub 侧**：`config.toml` 新增 `[mcp.<name>]`，把任意 stdio MCP server 挂进每个 agent 会话，排在协作工具之后、不覆盖它（`crates/hub/src/paths.rs` 解析，`crates/hub/src/collab.rs` 的 `prepare` 里 append）。端到端有测试：`crates/mock-agent/tests/codex_hub.rs::extra_mcp_servers_ride_along_with_the_collaboration_one`。
2. **豆包侧**：`integrations/doubao-mcp/`，一层 MCP 壳，只认 `tools/paint.mjs` 的命令行契约（`"<prompt>" --ref ... --out ... --json`），不碰插件内部实现。暴露 `doubao_paint` 与 `doubao_status`。

```toml
[mcp.doubao]
command = ["node", "C:/Users/Link/Dev/CrossAgents/integrations/doubao-mcp/server.mjs"]
env = { DOUBAO_PAINTER_HOME = "D:/tools/dsh-doubao-painter" }
```

还差的一步是**真实出图**：要先把 dsh-doubao-painter clone 下来、豆包带 `--remote-debugging-port=9225` 重启，然后让 agent 调一次。那一步取决于 CDP 与选择器在 2.32.6 上是否仍然有效，不是代码问题。

## 顺带一个白捡的

README 提到的 `tools/skill.mjs` 和我在 [doubao-client.md](doubao-client.md) 里挖到的是同一件事：技能目录里 46 个带 `scripts/` 的技能，其中视频下载、转码、PDF 处理这类是**纯本地 Python**，不走 UI、不消耗对话额度。生图、云端媒体处理、飞书妙记那些则依赖运行时注入的凭证，脱离豆包跑不通——和 README 的结论一致。
