# Claude Code 原生驱动实测：stream-json 控制协议

> 2026-10-22 · Claude Code **2.1.304**（桌面版自带的 `claude.exe`，Windows x64，Pro 订阅）· `@anthropic-ai/claude-agent-sdk` **0.3.307** · 对应 [DESIGN.md](../DESIGN.md) §9、§14
>
> 实测脚本：[claude-stream-json-probe.mjs](claude-stream-json-probe.mjs)（主流程 + `--followup` 补测，用 haiku 模型）。
> 协议定义来自 SDK 包里的 `sdk.d.ts`，行为结论来自真实账号上的运行记录。

## 1. 结论：两条路其实是一条，Rust 直接说 stream-json

Agent SDK 不是另一套接口。它（`sdk.mjs` / `core.mjs`）做的就是启动 `claude` 并加上：

```
-p --output-format stream-json --verbose --input-format stream-json
--permission-prompt-tool stdio   # 权限请求走 stdin/stdout 的控制消息
```

然后在 stdin/stdout 上收发 JSON 行：用户消息、`control_request` / `control_response`。SDK 的所有能力（权限回调、中断、换模型、文件回退、用量……）都是这套控制协议上的请求。

所以 **`claude-native` 用 Rust 直接说这套协议，不需要 Node 侧车**。跟 `codex_native` 跟着 `codex app-server` 的协议走是同一种做法。代价是控制协议没有正式文档，以 SDK 的 `sdk.d.ts` 为准，升级 Claude Code 时核对一遍。

与 Codex 的结构差别：**一个进程一个会话**（Codex 一个 app-server 进程可开多个线程）。

## 2. 能力对照（全部已实测）

| 能力 | 怎么做 | 结论 |
|---|---|---|
| 握手 | `control_request{subtype:"initialize"}` | 返回命令、agent、模型、账号、版本、当前权限模式、能力等；未登录也能用 |
| 模型列表 | `list_models` | 别名（default / opus / sonnet / haiku…）、实际模型 id、价格说明、支持的 effort 档位、快速模式 |
| 发消息 | stdin 写 `{"type":"user","uuid":…,"session_id":…,"parent_tool_use_id":null,"message":{"role":"user","content":…}}` | **`uuid` 可以由 hub 指定**，之后的回退、分叉都用它；加 `--replay-user-messages` 会回显 |
| 输出流 | `system/init`（cwd、session id、工具、MCP、模型、权限模式）→ `assistant`（每个内容块一条：`text` / `thinking` / `tool_use`）→ `user`（`tool_result`）→ `rate_limit_event` → `result` | 另有 `system/task_started`、`task_notification`、`thinking_tokens`、`post_turn_summary` 等；加 `--include-partial-messages` 才有逐字的 `stream_event` |
| 工具结果 | `user` 消息的 `tool_use_result` | **带结构化结果**：shell 有 `stdout` / `stderr` / `interrupted`；写文件有 `type:"create"`、`content`、`originalFile`、`structuredPatch`；读文件有行号范围。直接能填 `ToolDetail`，不用解析文本。`tool_result_meta` 还有授权来源（config / mode / host） |
| 权限回调 | CLI 发 `control_request{subtype:"can_use_tool", tool_name, input, permission_suggestions, decision_reason…}`；hub 回 `{behavior:"allow", updatedInput}` 或 `{behavior:"deny", message}` | 允许和拒绝都生效。拒绝时模型收到 `is_error` 的 `tool_result`，内容就是 hub 给的 message，文件未被删。只读命令（`echo`、读文件）在 default 模式下不会来问 |
| 中断 | `interrupt` | 立即返回 success；本轮 `result` 的 subtype 为 `error_during_execution`，从发消息到收尾 4.5s（其中 3s 是故意等的） |
| 换模型 | `set_model{model}` | success；下一轮生效 |
| 恢复 | 新进程 `--resume <session_id>` | 记得之前的对话，session id 不变 |
| 分叉 | `--resume <id> --fork-session --resume-session-at <uuid>` | 得到新 session id，只含截止那条消息的历史。**`--resume-session-at` 要给助手消息的 uuid**：给用户消息的 uuid 时，那条消息会被重新执行一遍 |
| 文件检查点与回退 | 环境变量 `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING=1`；`rewind_files{user_message_id, dry_run?}` | `dry_run` 返回 `canRewind`、改动的文件和行数；真回退后被删掉的 note.txt 恢复为 v1（回到第 2 轮之前）。**比 Codex 多一项**：Codex 回退只改历史，不撤销文件 |
| 额度：每轮 | `rate_limit_event.rate_limit_info` | `status`（allowed / allowed_warning / rejected）、触发的窗口类型和利用率（0–1）、`unifiedWindows`（five_hour、seven_day 的利用率和重置时间，Unix 秒）、超额状态及关闭原因 |
| 额度：主动读 | `get_usage{skip_behaviors:true}` | `subscription_type`（pro）；`rate_limits.limits[]`：每项 `kind`（session / weekly_all …）、`group`、`percent`、`resets_at`、`severity`、`is_active`；`model_scoped[]`（按模型的周额度）；`extra_usage` 与 `spend`（额外用量的开关、上限、已用）；`seven_day_breakdown`（七天用量按 Claude Code / Chats / Cowork 分）。另有很多实验性的空字段，不要依赖 |
| 用量与花费 | `result.usage`、`result.total_cost_usd`；`get_usage.session.model_usage` | **`total_cost_usd` 是这个进程开始以来的累计**，不是本轮；按模型的 token、缓存读写、思考 token、上下文窗口都有 |
| 上下文占用 | `get_context_usage` | 按类别（系统提示、工具、MCP、消息……）给 token 数，和 Claude Code 的 `/context` 一样 |
| 会话与目录 | 在别的目录 `--resume` | **不绑定**：照样记得对话，`init.cwd` 变成新目录，session id 不变（与 Codex 一样，每次都能换 cwd）。会话文件在 `~/.claude/projects/<cwd 转义>/` 下，新目录下也会有一份 |

## 3. 对 CrossAgents 的含义

- **额度直接接上 N2 的用量页**：`get_usage.rate_limits.limits[]` → `AccountStatus.rate_limits`（五小时、七天）；`model_scoped` → `limits`；`extra_usage` / `spend` → `credits`；`subscription_type` → 套餐。每轮的 `rate_limit_event` 相当于 Codex 的 `account/rateLimits/updated`。
- **工具详情不用猜**：`tool_use_result` 是结构化的，比 ACP 的 `tool_call_update` 内容全，命令输出、文件 diff 都现成。
- **回退可以连文件一起撤**：H1 的「回退到某一轮」对 Claude 可以做到撤销文件，Codex 做不到，界面上要区分。
- **Windows 上 shell 工具叫 `PowerShell`**（`Bash` 也在）。权限组按工具名匹配时两个都要算 shell。
- **工具和配置会继承用户自己的 Claude Code 设置**：实测 `init.tools` 里有用户装的插件、MCP（claude.ai 连接器）和技能。hub 拉起的 agent 应该用 `--setting-sources` / `--strict-mcp-config` / `--tools` 收窄，只给 hub 的 MCP 和需要的内置工具。
- **账号**：`CLAUDE_CONFIG_DIR` 指向一个目录即一份独立配置（已实测：空目录是未登录状态），和 Codex 的 `CODEX_HOME` 一样一个账号一个目录。登录用 `claude auth login`，必须走一次浏览器授权；桌面版 Claude 的登录不写到 `~/.claude`，单独运行的 `claude.exe` 用不上。
- **代理**：`claude.exe` 不读 Windows 系统代理，本机（系统代理 `127.0.0.1:7890`）直连 claude.com 超时，设 `HTTPS_PROXY` 后正常。hub 启动 Claude（以及 Codex）时要传代理：默认读系统代理设置，账号上可以单独配置。
- **花费按进程累计**：hub 记录每轮花费时要用前后差值。

## 4. `claude_native` 驱动（已完成，见 [N3](../tasks/N3.md)）

照 `codex_native` 的结构：

1. `wire.rs`：stream-json 消息与控制请求的类型、翻译成 `SessionEventKind`（文本、思考、工具开始与结束、`ToolDetail`、用量、额度、警告）。
2. `mod.rs`：一个会话一个进程；`initialize`；发消息时指定 uuid；`can_use_tool` 转给 hub 的权限流程；中断、换模型、压缩（发 `/compact`）；恢复 `--resume`；结束时 `get_usage` 刷新额度。
3. `plugins`：`claude` 的默认驱动从 `acp-generic` 改为 `claude-native`；账号 provider `claude`（读 `get_usage`，按 `CLAUDE_CONFIG_DIR` 区分账号；本机 `~/.claude` 已登录时自动登记）。
4. 代理配置（hub 层，Codex 和 Claude 共用）。
5. 分叉与回退的 API 和界面（N2 剩下的一项）可以在这之后一起做。
