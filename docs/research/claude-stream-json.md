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

## 5. 轮中插话（steer）实测

> 2026-11-05 · Claude Code **2.1.317**（本机 bundled CLI：`C:\Users\Link\AppData\Local\Claude-Data\claude_pzs8sxrjxfjjc\claude-code\2.1.317\a33aa5518497\claude.exe`）· 模型 `haiku` · `HTTPS_PROXY=http://127.0.0.1:7890`
>
> 探针：`python -I <scratchpad>\claude_steer_probe.py <claude路径> during|boundary|late`；另用同结构的 Node 探针复跑了全部场景，以及一个 interrupt 场景。下面的秒数是相对进程启动的时间，事件做了字段裁剪。

启动参数（与 `claude_native` 驱动一致）：

```
claude -p --input-format stream-json --output-format stream-json --verbose \
  --replay-user-messages --permission-prompt-tool stdio --include-partial-messages \
  --strict-mcp-config --setting-sources project,local --model haiku --session-id <uuid>
```

stdin 每条消息（`uuid` 由我们指定）：

```json
{"type":"user","uuid":"<我们的>","session_id":"<session>","parent_tool_use_id":null,
 "message":{"role":"user","content":"…"}}
```

### 5.1 `during`：工具执行中写入

第一轮让模型调用 Bash 运行 `sleep 12; echo LONGDONE`；工具执行中写入「立刻停止剩余步骤，只回复一行：STEERED-OK」。

```
 0.01 >>> 写第一条 prompt
 4.85 <<< assistant tool_use:Bash                 ← 工具开始
 7.86 >>> 写 steer（工具还在跑）
17.63 <<< user tool_result LONGDONE
17.64 <<< user isReplay=true uuid=804eed79…      ← 正是我们写的 uuid
19.75 <<< assistant text STEERED-OK
19.76 <<< result success result=STEERED-OK user_message_uuid=90f33ec1…（第一条 prompt）
```

**工具边界读到**，本轮最终答案被改写，**只有一个 result**。

### 5.2 `boundary`：第一个工具结果回来时写入

```
 4.92 >>> 写 steer（第一个 tool_result 那一刻）
 6.25 <<< assistant tool_use:Bash                 ← 还会继续发工具
 9.35 <<< user isReplay=true uuid=<steer>
 9.xx <<< assistant text STEERED-OK
     <<< result success user_message_uuid=<第一条 prompt>   ← 一个 result
```

写入后还会有一个工具调用，**回显要等下一次模型请求**，不是「写下就生效」。因此读到检测不能用「下一条 assistant 输出」——那条输出可能属于上一请求。

### 5.3 `late`：最后一次模型调用之后写入

```
11.44 <<< assistant text 最终答案                  ← 本轮答案已出
11.44 >>> 写 steer
11.46 <<< result success user_message_uuid=<第一条 prompt>   ← 本轮结束，答案未变
11.49 <<< system/init                              ← 新一轮
12.57 <<< user isReplay=true uuid=<steer>
12.58 <<< assistant text STEERED-OK user_message_uuid=<steer>
12.59 <<< result success user_message_uuid=<steer> result_index=1
```

**共 2 个 result**：旧的归第一条 prompt，新的归 steer。下一轮的 `system/init`/回显可能先于或晚于上一轮 `result`（本机这次是晚于；CodeBuddy 上是早于），所以 hub 不能只用「waiter 为空」判断结果归属。`result.user_message_uuid` 是可靠的归属字段。

### 5.4 `interrupt`：有未读插话时中断

```
 6.80 >>> 写 steer（工具还在跑）
 7.80 >>> control_request{subtype:"interrupt"}
 7.83 <<< control_response {subtype:"success", response:{still_queued:["4dcd732e…"]}}
 7.85 <<< result error_during_execution is_error=true user_message_uuid=<第一条 prompt>
       >>> 写一条新 prompt
19.89 <<< user isReplay=true uuid=4dcd732e…        ← 未读的 steer 仍被消费
19.92 <<< result success user_message_uuid=4dcd732e…
```

`interrupt` 的应答带 `still_queued`：**stdin 里未读的消息不会丢**，本轮结束后仍会被下一条运行读到。驱动因此不能在中断时直接把它标成已读或结束本轮：要像 `late` 一样等它被读、收到它自己的 result；否则那条 result 会变成孤儿，可能结束下一轮。若进程已经退出、`still_queued` 为空或没有这个字段，则无法保证会读，发一条 Warning 说明「这条插话没有被读取」。

### 5.5 对 `claude_native` 的含义

- `--replay-user-messages` 的回显（`isReplay: true`）**保留我们写的 uuid**，是唯一精确的「模型已读到」信号；CodeBuddy 不保留。
- `result.user_message_uuid` 标出这条 result 结束的是哪条用户消息；一条用户消息一个 result。
- 工具执行中写入：下一模型请求读到，改本轮答案；最后一次模型调用之后写入：另起一轮，属于同一次插话，驱动要把那一轮折进同一个 hub 轮次。
- 中断不丢未读消息（`still_queued`）；读到前不能谎报已读。

2026-11-05 补测 CodeBuddy 2.179.2（默认驱动参数、代理 `127.0.0.1:7890`）：流式输出开始后写入插话并立即发送 interrupt，控制应答表示已中断但没有 `still_queued`；随后看到一个 `success` result、一个新 `system/init`、再一个 `success` result。两条 result 都没有 `user_message_uuid`，也没有看到插话的 `isReplay` 回显。另用不存在的模型触发失败：第一条 `init` 后写入插话，第二条 `init` 先于 `error_during_execution` result，result 没有 `user_message_uuid`；观察 60 秒未见插话回显或第二个 result。这些事件不足以满足「已按原 uuid 回显的独立插话」的保持条件。上面的 result 归属结论针对 Claude Code。

### 5.6 stdin 背压与停滞

2026-11-05，Windows 本机、同一代理：让 Claude Code 2.1.317（haiku）和 CodeBuddy 2.179.2 调用 PowerShell `Start-Sleep -Seconds 15`，观察到 tool_use 后，向各自 stdin 以 4096 字节分块写入约 1 MiB 的 JSON 插话。Claude 写完用时 0.06 秒、相邻进展的最大间隔 0.05 秒；CodeBuddy 写完用时 0.01 秒、最大间隔小于 0.01 秒。这仅是这两版 CLI、这次运行的测量，不能保证所有版本和环境的延迟。驱动和 runtime host 采用 20 秒无字节进展期限，覆盖实测 15 秒工具执行并留有余量；每次写入进展都重置期限，整帧总耗时可以更长。停滞时关闭管道并杀进程，避免半帧之后继续使用流。

新 hub 接管升级前已运行的旧 runtime host 时，仍使用旧 host 的写入实现：其真实 stdin 卡住时可能无法处理 Kill/CloseStdin，也不会按新期限报告退出。协议没有版本协商，hub 无法远程给旧进程补上此保证；需要结束旧 host 并用新版本重新启动会话。新建 host 才有上述进展式停滞保护。

同日进一步测量：两种 CLI 均确认调用 `Start-Sleep -Seconds 60` 后，等待 1 秒，再以 4096 字节为块发送 4,194,505 字节的 JSON 插话。Claude 2.1.317 写完用时 0.047 秒、最大块间隔 0.001 秒；CodeBuddy 2.179.2 用时 0.049 秒、最大块间隔不足 0.001 秒。测得的是进程 stdin 的读取进展，并不代表插话已被模型处理；在工具运行期间完成写入后结束了探针进程。这些测量支持上述无进展期限，但不能证明所有版本或所有负载的最大背压时间。
