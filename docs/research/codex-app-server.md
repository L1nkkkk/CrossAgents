# codex app-server 实测报告

> 2026-10-21 · codex-cli **0.158.0**（npm `@openai/codex`，Windows x64）· 对应 [DESIGN.md](../DESIGN.md) §9、§14
>
> 实测脚本：[codex-app-server-probe.py](codex-app-server-probe.py)（主流程 + `--followup` 补测）。
> 方法清单来自本机 `codex app-server generate-json-schema` / `generate-ts` 生成的协议定义，
> 行为结论来自真实账号上的运行记录。未经实测的项目标「未确认」。

## 1. 结论速览

| 能力 | 结论 | 依据 |
|---|---|---|
| 一个进程多个线程 | **支持** | 同一进程开 2 个线程，`thread/loaded/list` 返回两个 id；两个线程的轮次并发执行，3.3s / 3.6s 同时完成 |
| 新建线程（cwd、沙箱、审批、开场说明、MCP） | **支持** | `thread/start` 的 `cwd`、`sandbox`、`approvalPolicy`、`developerInstructions`、`config`；开场说明写进 rollout 的 `developer` 消息 |
| 发消息 / 流式输出 | **支持** | `turn/start` → `item/agentMessage/delta` … `turn/completed` |
| 中断 | **支持** | `turn/interrupt{threadId,turnId}` 返回 `{}`，10ms 内 `turn/completed.status = "interrupted"` |
| 关进程后恢复 | **支持** | 杀掉进程、新进程 `thread/resume{threadId}` 后继续对话，模型答出第一轮要求的 `PONG` |
| 分叉 | **支持** | `thread/fork{threadId,lastTurnId}` 得到新线程，`forkedFromId` 指回原线程，只含截止那一轮 |
| 回退对话 | **支持** | `thread/revert{threadId,beforeTurnId}` 后 `thread/turns/list` 从 2 轮变 1 轮 |
| 回退时撤销文件 | **不支持** | 协议注释：“This only changes persisted conversation history. It does not revert local file changes.” |
| 按轮文件改动 | **支持** | `turn/diff/updated` 给出本轮累计 unified diff；`fileChange` 条目给出每个文件的 diff |
| 权限 / 审批请求 | **支持** | 服务端请求 `item/commandExecution/requestApproval`、`item/fileChange/requestApproval`、`item/permissions/requestApproval`；实测收到命令审批并回 `decline` 生效 |
| token 用量 | **支持** | `thread/tokenUsage/updated`：`total` / `last` 分项 + `modelContextWindow` |
| 限额 | **支持** | `account/rateLimits/updated` 通知（每次模型调用后）+ `account/rateLimits/read` |
| 换模型、推理强度 | **支持** | `turn/start` 的 `model`、`effort` 对本轮及之后生效；`model/list` 列出可用模型和强度 |
| MCP 服务器 | **支持（要加一项配置）** | `thread/start.config.mcp_servers.<name>` 按线程挂载；在 `approvalPolicy = "never"` 下必须同时设 `default_tools_approval_mode = "approve"`，否则调用被拒 |
| 压缩上下文 | 支持（未实测） | `thread/compact/start` |
| 线程与 cwd 绑定 | **不绑定** | `thread/resume` 传另一个 `cwd`，返回的 `thread.cwd` 随之改变；`turn/start` 也可改 cwd |

**没有发现需要改设计方向的问题。** 需要降级的只有「回退时撤销文件」（能力表标 `revert_files = false`）。

## 2. 启动与握手

- 启动：`codex app-server`（默认 `--listen stdio://`）；JSON-RPC 2.0，每行一条 JSON。帮助里标着 `[experimental]`。
- Windows 上 `codex` 是 npm 生成的 `codex.cmd` / `codex.ps1`，背后是 `node bin/codex.js`，它再起原生程序
  `node_modules/@openai/codex/node_modules/@openai/codex-win32-x64/vendor/x86_64-pc-windows-msvc/bin/codex.exe`。
  `codex.js` 只多设了几个包管理器提示变量，所以驱动直接启动 `codex.exe`，避免杀掉 `cmd.exe` 后留下孤儿进程。
- 握手：请求 `initialize{clientInfo, capabilities}`，再发通知 `initialized`。
  - `clientInfo.name` 会成为 rollout 里的 `originator` 和 User-Agent 前缀。驱动用 `crossagents`，M5 的用量归属就能识别。
  - `capabilities.experimentalApi = true` 才能用实验方法（多出 63 个，如 `thread/settings/update`、`turn/settings/update`）。本阶段不用。
  - 响应：`userAgent`、`codexHome`（实际生效的 `CODEX_HOME`）、`platformFamily`、`platformOs`。
- 服务端发的消息不带 `"jsonrpc":"2.0"` 字段，通知多一个顶层 `emittedAtMs`；服务端请求的 id 是整数，从 0 开始。

## 3. 方法与事件对照表

非实验接口共 104 个客户端方法、10 个服务端请求、83 个通知。下面只列本项目用到或相关的部分。

### 3.1 客户端 → 服务端

| 需要 | 方法 | 关键参数 | 实测 |
|---|---|---|---|
| 初始化 | `initialize` + 通知 `initialized` | `clientInfo{name,title,version}`、`capabilities{experimentalApi,requestAttestation}` | ✓ |
| 新建线程 | `thread/start` | `model`、`cwd`、`approvalPolicy`、`sandbox`、`config`（任意 config.toml 键）、`baseInstructions`、`developerInstructions`、`ephemeral` | ✓ |
| 发消息（开始一轮） | `turn/start` | `threadId`、`input: UserInput[]`、`model`、`effort`、`summary`、`cwd`、`approvalPolicy`、`sandboxPolicy`、`outputSchema` | ✓ |
| 轮中插话 | `turn/steer` | `threadId`、`input`、`expectedTurnId` | ✓ 0.166.0-alpha.5.1：返回 `{turnId}`（仍是同一个轮次）；正在跑的轮次下一次模型调用读到新输入并把回答改成新内容。轮次结束后再发报 `{"code":-32600,"message":"no active turn to steer"}`，`expectedTurnId` 不对时同样被拒 |
| 中断 | `turn/interrupt` | `threadId`、`turnId` | ✓ |
| 恢复线程 | `thread/resume` | `threadId`（或 `path` / `history`）、同 `thread/start` 的覆盖项、`excludeTurns` | ✓ |
| 分叉 | `thread/fork` | `threadId`、`lastTurnId`（含该轮）、覆盖项、`ephemeral` | ✓ |
| 回退 | `thread/revert` | `threadId`、`beforeTurnId`（去掉该轮及之后） | ✓ |
| 列出轮次 | `thread/turns/list` | `threadId`、`cursor`、`limit`、`sortDirection`（默认倒序） | ✓ |
| 读线程 | `thread/read` | `threadId`、`includeTurns` | — |
| 停止接收某线程事件 | `thread/unsubscribe` | `threadId` → `{status:"unsubscribed"}` | ✓（线程仍在 `loaded/list` 里，见 §6） |
| 已加载线程 | `thread/loaded/list` | — | ✓ |
| 归档 / 删除 | `thread/archive`、`thread/delete` | `threadId` | ✓ archive（rollout 移到 `archived_sessions/`） |
| 压缩 | `thread/compact/start` | `threadId` | — |
| 模型列表 | `model/list` | `includeHidden` | ✓ |
| 限额 | `account/rateLimits/read` | 无参数 | ✓ |
| MCP 状态 | `mcpServerStatus/list` | `threadId`、`detail` | ✓ |
| 账号 | `account/read`、`account/login/start` | — | 未用（M5 已有账号实现） |

### 3.2 服务端 → 客户端：请求（需要回复）

| 请求 | 用途 | 回复 |
|---|---|---|
| `item/commandExecution/requestApproval` | 执行命令前审批 | `{decision: "accept" \| "acceptForSession" \| "decline" \| "cancel" \| {acceptWithExecpolicyAmendment…}}`；请求里带 `availableDecisions` |
| `item/fileChange/requestApproval` | 改文件前审批 | `{decision: "accept" \| "acceptForSession" \| "decline" \| "cancel"}` |
| `item/permissions/requestApproval` | 申请额外权限（网络、目录） | `{permissions, scope}` |
| `mcpServer/elicitation/request` | MCP 服务器向用户要输入 | `{action, content, _meta}` |
| `item/tool/requestUserInput` | 实验：工具向用户提问 | — |
| `applyPatchApproval`、`execCommandApproval` | 旧版审批（v1 协议） | — |
| `account/chatgptAuthTokens/refresh`、`attestation/generate` | 外部令牌 / 证明，我们不声明相关能力就不会收到 | — |

回复后服务端发通知 `serverRequest/resolved{threadId, requestId}`。

### 3.3 服务端 → 客户端：通知

| 统一事件（§9.2） | 通知 | 说明 |
|---|---|---|
| 轮次开始 | `turn/started{threadId, turn}` | `turn.id` 是 UUIDv7 |
| 轮次结束 | `turn/completed{threadId, turn}` | `turn.status`：`completed` / `interrupted` / `failed`；失败时 `turn.error` |
| 文本流 | `item/agentMessage/delta{threadId,turnId,itemId,delta}` | 完整文本在 `item/completed` 的 `agentMessage.text` |
| 思考 | `item/reasoning/summaryTextDelta`、`item/reasoning/textDelta` | 实测 `effort = low/medium` 时没有出现（未确认何时出现） |
| 工具调用 | `item/started` / `item/completed`，`item.type` 为 `commandExecution`、`mcpToolCall`、`fileChange`、`webSearch`、`dynamicToolCall` 等 | 状态在 `item.status`；命令输出另有 `item/commandExecution/outputDelta` |
| 按轮文件改动 | `turn/diff/updated{threadId,turnId,diff}` | 本轮**累计** unified diff，每次改动后整体重发 |
| 用量 | `thread/tokenUsage/updated{threadId,turnId,tokenUsage{total,last,modelContextWindow}}` | `total` 是线程累计；`last` 是最近一次模型调用 |
| 限额 | `account/rateLimits/updated{rateLimits}` | 账号级，不带 `threadId`；稀疏更新，应与 `account/rateLimits/read` 合并 |
| 错误 | `error{error, willRetry, threadId, turnId}` | `willRetry = false` 时随后有 `turn/completed(status=failed)` |
| 警告 | `warning`、`configWarning`、`deprecationNotice` | 如 config.toml 里的模型或 `service_tier` 不可用 |
| 线程状态 | `thread/status/changed{threadId,status}` | `idle` / `active` / `notLoaded` / `systemError` |
| 其他 | `thread/started`、`mcpServer/startupStatus/updated`、`account/updated`、`skills/changed`、`remoteControl/status/changed` | 驱动忽略 |

## 4. 各项能力的依据（展开）

**恢复。** rollout 存在 `<CODEX_HOME>/sessions/YYYY/MM/DD/rollout-<时间>-<threadId>.jsonl`，`thread.path` 给出路径（标 UNSTABLE）。
`thread/resume` 只需 `threadId`，服务端按 id 找 rollout；找不到时报错
`{"code":-32600,"message":"no rollout found for thread id …"}`。`thread/revert` 会把历史写进新文件
（`…-<threadId>_<新 uuid>.jsonl`），之后仍能按 `threadId` 恢复，所以**只存线程 id，不存路径**。
恢复时 `model`、`sandbox`、`approvalPolicy`、`config`（含 MCP）要重新传，否则用 config.toml 的默认值。

**分叉。** `lastTurnId` 指定截止轮（含）；该轮不能在进行中。新线程**不继承**原线程的模型、沙箱、审批和 MCP 设置：
实测分叉出的线程变成 config.toml 的默认模型和 `readOnly` 沙箱。驱动必须在 `thread/fork` 里把这些全部重新传一遍。分叉可另给 `cwd`。

**回退。** `beforeTurnId` 指定的那一轮及之后全部从历史里去掉；只改历史，不动文件。只对 `historyMode = "paginated"` 的线程有效，0.158.0 新建线程都是 paginated。

**按轮文件改动。** `danger-full-access` 下让模型先建后改同一个文件：`turn/diff/updated` 出现 5 次，最后一次是整轮的累计 diff（新文件，内容 `two`）；
两个 `fileChange` 条目分别是 `add`（`one`）和 `update`（`-one +two`）。shell 命令写的文件不会产生 `fileChange` 条目，但会出现在轮次 diff 里（未确认，只在源码描述里见到）。

**审批。** `approvalPolicy = "untrusted"`、`sandbox = "read-only"` 时让模型执行 `echo probe > made.txt`，收到
`item/commandExecution/requestApproval`（含 `command`、`cwd`、`availableDecisions`），回复 `decline` 后命令未执行、模型照常结束本轮。

**用量与限额。** 每次模型调用后各发一次 `thread/tokenUsage/updated` 和 `account/rateLimits/updated`。限额样例见 §7。

**换模型。** `turn/start.model` / `effort` 对“本轮及之后”生效，所以 `set_model` 只需记下来、在下一次 `turn/start` 里带上。
`effort` 可选值按模型不同（`model/list` 的 `supportedReasoningEfforts`），本机账号：`low`/`medium`/`high`/`xhigh`/`max`（`gpt-5.6-terra` 多一个 `ultra`）。

**MCP。** `config` 接受任意 config.toml 键（JSON 形式），`{"mcp_servers": {"crossagents": {"command", "args", "env"}}}` 只对这个线程生效。
实测工具可见、`env` 生效（工具返回 `env:ok`）。但 `approvalPolicy = "never"` 下调用直接失败：
`MCP tool call requires approval, but approval policy is never`。加 `default_tools_approval_mode = "approve"` 后，
`workspace-write` 和 `danger-full-access` 两种沙箱下都调用成功。用户 config.toml 里已有的 MCP 服务器也会一起加载
（本机加载了 `codex_apps`、`node_repl` 等 6 个）。

## 5. §14 中与 Codex 有关的问题

| 问题 | 回答 |
|---|---|
| 一个进程能否承载多个线程 | 能。并发两个线程的轮次互不阻塞。稳定上限未压测（未确认）；每个线程会各自拉起一套 MCP 服务器进程，线程多时这才是主要开销 |
| 线程是否与 cwd 绑定 | 不绑定。cwd 是线程的设置，`thread/resume` / `thread/fork` / `turn/start` 都能改。CrossAgents 仍按 DESIGN 让一个 Worker 固定一个目录 |
| 恢复靠什么 | 线程 id。rollout 文件在 `CODEX_HOME/sessions/` 下，由服务端按 id 查找；回退后文件名会变，所以不要依赖路径 |
| 放行模式下对 cwd 外文件的限制 | `danger-full-access` + `approvalPolicy = "never"`：**没有限制**，实测写 cwd 外文件成功。`workspace-write`：写 cwd 外失败。注意 Windows 上 `workspace-write` 要先装好 Windows 沙箱，本机（config.toml 设了 `[windows] sandbox = "elevated"`）连 cwd 内都写不了，命令报 `helper_unknown_error: setup refresh had errors` |

## 6. 坑

1. **用户 config.toml 会渗进来。** 用默认 `CODEX_HOME`（`~/.codex`）时，线程继承用户配置的模型、`service_tier`、Windows 沙箱、MCP 服务器、skills。
   本机配置的 `model = "gpt-6-astra"` 对 ChatGPT 账号不可用，没显式传模型时每一轮都 400 失败。
   驱动应在 Profile 有模型时总是显式传；没有时沿用 Codex 默认，并把 `warning` 通知转成警告事件。M5 新建的独立账号目录没有这个问题。
2. **分叉不继承设置**（见 §4），恢复也一样：每次 `thread/start` / `resume` / `fork` 都要传全套覆盖项。
3. **MCP 工具在 `never` 下被拒**，必须给 crossagents 的服务器加 `default_tools_approval_mode = "approve"`。
4. **忙线程上的 `turn/start` 不排队，而是插话。** 对正在跑的线程再发 `turn/start`，返回的是**同一个**进行中的轮次 id，新输入并入当前轮（实测长输出被改成回答 `OK`）。hub 仍须自己按会话排队，一次一轮；要主动插话用 `turn/steer`（带 `expectedTurnId`），它在轮次结束后会明确报错，便于 hub 降级排队。
5. **`thread/unsubscribe` 不卸载线程**，只是本连接不再收它的事件；线程还在 `thread/loaded/list` 里，归档后才变成 `notLoaded`。真正释放靠进程退出（或空闲后服务端自行卸载，未确认）。
6. **Windows：不要通过 `codex.cmd` 启动**，杀掉 `cmd.exe` 不会带走 `node` 和 `codex.exe`。直接起 `vendor/…/codex.exe`。
7. **回退会生成新的 rollout 文件**，两个文件的 `session_meta.id` 相同。M5 按 rollout 文件统计用量时可能把同一线程算两次（待 M5 侧去重）。
8. `account/rateLimits/updated` 是账号级、稀疏的：字段为 null 不代表清零，要与上一次的值合并。
9. 审批请求的 `availableDecisions` 里不一定有 `decline`（实测只给了 `accept`、`acceptWithExecpolicyAmendment`、`cancel`），但回 `decline` 仍被接受。
10. 0.158.0 的 app-server 仍标 `[experimental]`，协议在快速变化（字段有 `deprecated` / `UNSTABLE` 标注，如 `thread.path`、`personality`）。驱动只用非实验接口，解析时对未知字段和未知通知宽容。

## 7. 真实样例（节选，已去掉账号标识）

初始化：

```json
→ {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"crossagents_probe","title":"CrossAgents probe","version":"0"},"capabilities":{"experimentalApi":false,"requestAttestation":false}}}
← {"id":1,"result":{"userAgent":"crossagents_probe/0.158.0 (Windows 10.0.26200; x86_64) unknown (crossagents_probe; 0)","codexHome":"C:\\Users\\…\\.codex","platformFamily":"windows","platformOs":"windows"}}
→ {"jsonrpc":"2.0","method":"initialized"}
```

新建线程（带 MCP）：

```json
→ {"id":3,"method":"thread/start","params":{"model":"gpt-6-luna","cwd":"…\\codex-probe\\a","approvalPolicy":"never","sandbox":"workspace-write",
   "developerInstructions":"You are a probe. Keep every answer under ten words.",
   "config":{"mcp_servers":{"probe_echo":{"command":"…\\python.exe","args":["…\\echo_mcp.py"],"env":{"PROBE_ENV":"ok"}}}}}}
← {"id":3,"result":{"thread":{"id":"01a15d96-e81e-7e53-a8c1-cdca0e533ce5","sessionId":"01a15d96-e81e-…","forkedFromId":null,"historyMode":"paginated",
   "model":"gpt-6-luna","status":{"type":"idle"},"path":"C:\\Users\\…\\.codex\\sessions\\2026\\10\\21\\rollout-2026-10-21T16-32-13-01a15d96-….jsonl",
   "cwd":"…\\codex-probe\\a","cliVersion":"0.158.0","originator":"crossagents_probe","source":"vscode","turns":[]},
   "model":"gpt-6-luna","modelProvider":"openai","cwd":"…","approvalPolicy":"never","approvalsReviewer":"user",
   "sandbox":{"type":"workspaceWrite","writableRoots":[],"networkAccess":false,"excludeTmpdirEnvVar":false,"excludeSlashTmp":false},"reasoningEffort":"high"}}
```

一轮的事件流（按到达顺序，省略重复的 delta）：

```json
→ {"id":4,"method":"turn/start","params":{"threadId":"01a15d96-…","effort":"low","input":[{"type":"text","text":"Call the probe_echo tool with text 'hello', then reply with exactly: PONG","text_elements":[]}]}}
← {"id":4,"result":{"turn":{"id":"01a15d96-e89d-…","items":[],"itemsView":"notLoaded","status":"inProgress","error":null,"startedAt":null,"completedAt":null,"durationMs":null}}}
← {"method":"thread/status/changed","params":{"threadId":"01a15d96-…","status":{"type":"active","activeFlags":[]}}}
← {"method":"turn/started","params":{"threadId":"01a15d96-…","turn":{"id":"01a15d96-e89d-…","status":"inProgress",…}}}
← {"method":"item/started","params":{"item":{"type":"userMessage","id":"…","content":[…]},"threadId":"…","turnId":"…","startedAtMs":1792571537351}}
← {"method":"thread/tokenUsage/updated","params":{"threadId":"…","turnId":"…","tokenUsage":{"total":{"totalTokens":14993,"inputTokens":14964,"cachedInputTokens":0,"cacheWriteInputTokens":0,"outputTokens":29,"reasoningOutputTokens":0},"last":{…},"modelContextWindow":258400}}}
← {"method":"account/rateLimits/updated","params":{"rateLimits":{"limitId":"codex","primary":{"usedPercent":5,"windowDurationMins":43200,"resetsAt":1795131666},"secondary":null,"credits":{"hasCredits":true,"unlimited":false,"balance":"53035.29"},"planType":"free","rateLimitReachedType":null}}}
← {"method":"item/started","params":{"item":{"type":"mcpToolCall","id":"exec-5f7a…","server":"probe_echo","tool":"probe_echo","status":"inProgress","arguments":{"text":"hello"}},…}}
← {"method":"item/completed","params":{"item":{"type":"mcpToolCall","id":"exec-5f7a…","status":"failed","error":{"message":"MCP tool call requires approval, but approval policy is never"}},…}}
← {"method":"item/agentMessage/delta","params":{"threadId":"…","turnId":"…","itemId":"msg_0910…","delta":"P"}}
← {"method":"item/completed","params":{"item":{"type":"agentMessage","id":"msg_0910…","text":"PONG","phase":"final_answer"},…}}
← {"method":"turn/completed","params":{"threadId":"…","turn":{"id":"01a15d96-e89d-…","status":"completed","error":null,"startedAt":1792571533,"completedAt":1792571548,"durationMs":15107,…}}}
```

中断：

```json
→ {"id":10,"method":"turn/interrupt","params":{"threadId":"01a15d96-…","turnId":"01a15d97-8f3c-…"}}
← {"id":10,"result":{}}
← {"method":"turn/completed","params":{"threadId":"01a15d96-…","turn":{"id":"01a15d97-8f3c-…","status":"interrupted",…}}}
```

审批：

```json
← {"id":0,"method":"item/commandExecution/requestApproval","params":{"kind":"command","threadId":"01a15d97-9ca7-…","turnId":"…","itemId":"exec-8173…",
   "command":"\"C:\\\\Windows\\\\System32\\\\WindowsPowerShell\\\\v1.0\\\\powershell.exe\" -Command 'echo probe > made.txt'","cwd":"…\\codex-probe\\c",
   "availableDecisions":["accept",{"acceptWithExecpolicyAmendment":{…}},"cancel"]}}
→ {"jsonrpc":"2.0","id":0,"result":{"decision":"decline"}}
← {"method":"serverRequest/resolved","params":{"threadId":"01a15d97-9ca7-…","requestId":0}}
```

分叉、回退、恢复：

```json
→ {"id":17,"method":"thread/fork","params":{"threadId":"01a15d96-…","lastTurnId":"01a15d96-e89d-…"}}
← {"id":17,"result":{"thread":{"id":"01a15d97-dfa9-…","forkedFromId":"01a15d96-…","model":"gpt-6-astra","turns":[{"id":"01a15d96-e89d-…",…}]},"sandbox":{"type":"readOnly",…},…}}
→ {"id":19,"method":"thread/revert","params":{"threadId":"01a15d96-…","beforeTurnId":"01a15d97-8f3c-…"}}
← {"id":19,"result":{"thread":{"id":"01a15d96-…","path":"…\\rollout-2026-10-21T16-33-16-01a15d96-…_01a15d97-e029-….jsonl","turns":[]},"turnsBackwardsCursor":"…","itemsBackwardsCursor":"…"}}
（杀掉进程，新进程握手后）
→ {"id":2,"method":"thread/resume","params":{"threadId":"01a15d96-…","excludeTurns":true,"approvalPolicy":"never","sandbox":"workspace-write"}}
← {"id":2,"result":{"thread":{"id":"01a15d96-…","status":{"type":"idle"},…},"model":"gpt-6-luna","cwd":"…\\codex-probe\\a",…}}
→ turn/start "What exact word did I ask you to reply with in my first message?"  →  回答 "PONG"
```

按轮 diff：

```json
← {"method":"turn/diff/updated","params":{"threadId":"…","turnId":"…","diff":"diff --git a/notes.txt b/notes.txt\nnew file mode 100644\n…\n--- /dev/null\n+++ b/notes.txt\n@@ -0,0 +1 @@\n+two\n"}}
← {"method":"item/completed","params":{"item":{"type":"fileChange","id":"exec-3016…","changes":[{"path":"…\\notes.txt","kind":{"type":"update","move_path":null},"diff":"@@ -1 +1 @@\n-one\n+two\n"}],"status":"completed"},…}}
```

## 8. 未确认

- 思考流（`item/reasoning/*`）在什么设置下出现；本次 `low` / `medium` 强度都没有收到。
- 单进程线程数的稳定上限；空闲线程是否会被服务端自动卸载（`thread/closed`）。
- `turn/steer` 与「对忙线程发 `turn/start`」的区别。
- shell 命令改的文件是否计入 `turn/diff/updated`。
- Windows 沙箱安装后（`windowsSandbox/setupStart`）`workspace-write` 的实际边界。
