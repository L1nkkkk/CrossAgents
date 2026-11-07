# 扩展（插件）设计稿

> 状态：E1 到 E4 已实现并走过真实的端到端验收（§8）。扩展 api 2 只有设计（§9），W1–W4 未开工。
> 目标是**随手定制**：在对话里让 agent 写一个工具、一段提示词或一个面板，
> 不编译、不重启 hub、不发版就能用上。
>
> 接口形状以 [crates/api-types/src/lib.rs](../crates/api-types/src/lib.rs) 的 `Extension*` 类型、
> [crates/core/src/event.rs](../crates/core/src/event.rs) 的 `extension.changed`、`extension.call.finished`
> 和 [crates/hub/src/access.rs](../crates/hub/src/access.rs) 的分级为准；改形状先改那里，再改这份文档和
> [API-AUTH.md](API-AUTH.md)。
>
> 思路来自 dsh 的创造模式（查接口 → 写插件 → 装上立刻生效），但实现不同：dsh 全栈 TS，靠
> Cordis 和 Node loader 在进程内热替换；hub 是 Rust，做不到，也不需要。

## 1. 边界

**留在 hub 里（要编译）**：事件日志、任务与合并状态机、驱动、权限、存储。这些有跨步骤的不变量，
拆到进程外只会多出跨进程事务。

**放进扩展（不编译）**：新工具、给模型的附加说明、事件响应、GUI 面板和卡片、主题。

现有内置功能不搬。等接口用熟了，再逐个判断有没有搬的价值。

**命名**：`plugin` 在代码里已经指 agent 描述文件（`~/.crossagents/plugins/<name>/plugin.toml`、
`PluginInfo`、`/v1/plugins`），而 `/v1/plugins` 的每一项都被当成一个 agent。所以新机制在代码里叫
`extension`，放在单独的目录，两边互不影响。界面上可以继续叫「插件」。

## 2. 一个扩展长什么样

```text
~/.crossagents/extensions/standup/
├── extension.toml
├── report.mjs
├── lead.md
└── gui/index.js
```

这是**源目录**，agent 和人都在这里改，hub 不直接执行它。真正运行的是启用时保存的快照：

```text
~/.crossagents/extension-versions/standup/<指纹>/    只读快照，工具、说明、GUI 都从这里读
~/.crossagents/extension-data/standup/               扩展自己的状态，跨版本保留，不计入指纹
~/.crossagents/extensions.json                       启用状态（§6）
```

指纹是所有文件路径和内容的哈希。符号链接直接拒绝（它能指向目录外，让指纹失去意义）。快照上限 50 MB，
超过就拒绝启用并说明原因。每个扩展保留当前版本和上一个版本，更早的在没有调用占用时清理。

**指纹算在快照上，不算在源目录上。** 先把源目录复制成快照，再对快照算哈希。反过来做的话，agent 在
「算完哈希」和「复制完」之间改了文件，指纹和真正会执行的内容就对不上，人看的是一份、跑的是另一份。
这也是 `ExtensionVersion.fingerprint` 的定义：它标识的始终是一份已经定型的快照。

```toml
name = "standup"
api = 1
description = "汇总今天各 run 的进展"
# 这个扩展拿到的 hub token 级别：read 或 write，默认 read
access = "read"

# 工具：调一次起一个进程
[[tools]]
name = "report"                  # 模型看到 standup__report
description = "汇总今天的进展"
groups = ["lead", "conversation"]
command = ["node", "report.mjs"]
input_schema = { type = "object", properties = { days = { type = "integer" } } }
# timeout_secs = 60

# 给模型的附加说明：追加在 hub 自己的说明之后，不能替换
[[prompts]]
groups = ["lead"]
file = "lead.md"

# 流式规则：和项目 .crossagents/rules/*.md 同一种文件，会话开始时读
[[rules]]
file = "rules/no-sleep.md"

# 事件响应：同样是调一次起一个进程，只观察不拦截（E4，见 §3）
[[on]]
event = "run.landed"
command = ["node", "notify.mjs"]
# timeout_secs = 60

# 常驻进程，确实需要时才写（E4，见 §3）
# [service]
# command = ["node", "server.mjs"]

[gui]
entry = "gui/index.js"
```

## 3. 工具：一次调用一个进程

这是和 dsh 差别最大的地方，也是“随手”的来源：**没有常驻进程，就不需要热替换**。切换到新版本后，下一次调用
跑的就是新代码。任何语言都能写，崩了也只影响这一次调用。

协议：

- stdin 收一个 JSON：`{ "tool": "report", "input": {...}, "caller": {...} }`。`caller` 由 hub 填写（见下文）。
- stdout 回一个 JSON，退出码 0 表示成功。`images` 字段沿用现有约定（`tool_content`，`crates/cli/src/cmd/mcp.rs`）。
- 退出码非 0：stderr 的内容作为工具错误返回给调用方。
- 环境变量：`CROSSAGENTS_URL`、`CROSSAGENTS_TOKEN`（见下文）、`CROSSAGENTS_EXTENSION_DATA`，
  再加 hub 自己的代理变量（`claude.com` 这类地址没代理到不了）。
- 进程的工作目录是快照目录，调用方的目录在 `caller.cwd`。快照只读，要落盘就写数据目录。
- 不经过 shell：`command` 的第一项和其余参数原样交给操作系统。
- hub 自己环境里的代理变量（`HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY`、`NO_PROXY`，大小写都认）原样带给工具；
  agent 的会话 token（`CROSSAGENTS_AGENT_TOKEN`）不带。
- 没有 `input` 当作 `{}`，再按 `input_schema` 校验。
- 成功时 stdout 解析出的 JSON 就是结果；退出码 0 但 stdout 不是 JSON（包括空）算失败。
- 结果里的 `stderr` 是最后 64 KB，成功时也带。

### 运行限制

| 项 | 规则 |
|---|---|
| 超时 | 默认 60 秒，上限 10 分钟；到点用 Job Object 杀掉进程树，复用 [crates/core/src/process/job.rs](../crates/core/src/process/job.rs) 的 `Job`（`new` → `assign(pid)` → `terminate`，非 Windows 上是空实现，Unix 用 `kill_group`） |
| stdout | 上限 1 MB，超过就杀掉进程并报错，不截断（截断的 JSON 没有意义） |
| stderr | 只保留最后 64 KB，不影响进程运行；完整输出写日志，单文件 10 MB 滚动 |
| 并发 | 每个扩展 4 个、全局 16 个；超出立刻返回“忙”（HTTP 429，错误码 `conflict`），不排队（排队会让调用方的超时更难判断） |

正在执行的调用：

- **切换版本**：已开始的调用在旧快照上跑完，新调用用新版本。调用占用的快照不会被清理。
- **停用或卸载**：立刻拒绝新调用，已开始的调用 5 秒后杀掉。Windows 上 Job Object 只能强杀，工具不能指望收到退出通知。
  现在只有停用（`Registry::disable`，经 `on_disabled` 通知运行器）；源目录删掉不算卸载，当前版本照旧可调。
- **hub 退出**：全部杀掉。结果丢失，工具要自己保证重跑安全，hub 不做重试。
- **取消**（`DELETE /v1/extensions/calls/{id}`）：立刻杀进程树，状态 `cancelled`。已经结束的调用，`DELETE` 原样回它的结果。

日志在 `<home>/extension-logs/<扩展名>.log`：每次调用一行开头、一行结尾（状态和耗时），中间是 stderr 的全部内容；
超过 10 MB 就把旧文件改名为 `<扩展名>.log.1`（覆盖更早的）再开新文件。

进程用 [crates/core/src/process.rs](../crates/core/src/process.rs) 的 `spawn_tree_strict` 起：Windows 上先挂起、
放进 Job Object 再放行（`job.rs` 的 `Job`），Unix 上自成一个进程组；起不了进程树就不运行。

### 接进 hub

| 位置 | 现状 | 改动 |
|---|---|---|
| `orchestrator/src/tools.rs` `TOOLS` | 静态表，`&'static str` | 不动，内置工具照旧 |
| `tools_for` | 只读 `TOOLS` | 追加已启用扩展的工具，按 `groups` 过滤 |
| `call_tool` | 在 `TOOLS` 里找不到就报 `NotFound` | 带 `__` 的名字转给扩展运行器 |
| `Orchestrator` | 不知道扩展 | 注入 `ExtensionHost`（`orchestrator/src/host.rs`，照 `AgentHost`），`Orchestrator::set_extensions` 设一次；运行器 `hub/src/extensions/runner.rs` 实现它 |
| 依赖 | 没有 JSON Schema 校验库 | `jsonschema`（hub），描述文件校验和每次调用的输入校验共用 |

### 命名

扩展工具用 `<扩展名>__<工具名>` 命名，不会和内置工具撞名。校验函数在
[crates/api-types/src/lib.rs](../crates/api-types/src/lib.rs)（`check_extension_name`、
`check_extension_tool_name`、`extension_tool_name`），hub、`extension_check` 和 GUI 共用一份：

| 项 | 规则 | 为什么 |
|---|---|---|
| 扩展名 | `^[a-z][a-z0-9-]*$` | 它同时是目录名和 `crossagents-ext:` 的主机名；不许下划线，所以名字里不可能出现 `__` |
| 工具名 | `^[a-z][a-z0-9_]*$`，且不含 `__` | 允许下划线（和内置工具一致），但 `__` 是分隔符，出现就无法唯一拆分 |
| 合起来 | `<扩展名>__<工具名>` 总长 ≤ 46 | MCP 客户端还会加 `mcp__crossagents__`（18 字符）前缀，有几家把工具名限到 64，46 是留给我们的部分 |

不合规直接 400，并在消息里写明哪一条。

### 每次调用都要检查

列表过滤只是展示。直接调用、`extension_call`、GUI 三个入口走**同一个函数**，执行时依次检查，和内置工具在
`call_tool` 里的做法一致：

1. 扩展存在且已启用，并在此刻确定这次调用用哪个版本。
2. 调用方有权限：agent 看当前 group，人看工具是否声明了 `human`。
3. 输入符合 `input_schema`。
4. 没有超过并发上限。

### 调用方由 hub 决定

`caller` 从凭据推出，请求体只能带 `input`，不能自己填身份（请求体里多写的 `caller` 会被忽略）：

- 经 `/v1/me/tools/*` 来的是 agent：`{ "kind": "agent", "id", "name", "groups", "cwd", "run", "task" }`。
  `groups` 是调用那一刻 hub 算出来的，`run`、`task` 来自它当前持有的认领，没有就是 `null`。
- 经 GUI 路由来的是人：`{ "kind": "human" }`。工具要在 `groups` 里写了 `human` 才能被 GUI 调用。
- 工具的 `groups` 为空，谁都调不了（既不是 agent 的任何组，也不含 `human`）。

**只有人的凭据才算 `human`。** `POST /v1/extensions/{name}/tools/{tool}` 只接受 `Grant::Level`（总 token、只读 token 里
够 write 的那种）；扩展自己的 token（`Grant::Extension`）调这条路由一律 403，不管它是 read 还是 write。否则一个
write 级的扩展拿着自己的 token 调这条路由，hub 会把它当成人，它就能调只开放给 `human` 的工具，等于冒充了人。
这条在两处挡：`access::refusal`（策略点）和路由处理函数里再检查一次。agent 的会话 token 本来就只到 `/v1/me*`。

同页面运行的 GUI 扩展之间互相分不开（§5），所以 `ca.tools.call` 限定“只调自己的工具”只是约定，不是安全边界。
隔离 webview 里这一条是真边界，见 §9「Webview」。

### 扩展的 token

`grant_of`（`crates/hub/src/api.rs`）原来只认总 token、只读 token 和会话 token，没有可以签发的 write 凭据，E1 新做了：

- 每次调用签发一个随机 token（`ca-ext-…`），只存在 hub 内存里，进程树结束、结果可见之前就作废；hub 重启后全部失效。
- `Grant` 增加 `Extension { name, level }`，`level` 的类型是 `ExtensionAccess`，只能是 read 或 write，不能是 admin。
  走 `access.rs` 现有的策略点，按声明的级别放行；`Grant` 因此不再是 `Copy`，`refusal` 改成接受 `&Grant`。
  拒绝的消息里写明是哪个扩展的 token，通过的请求也在日志里记下扩展名。
- 扩展的 token 到不了 `/v1/me*`（那一组只认会话 token，回 401），到不了 admin 路由，也调不了上面那条「以人的身份调工具」的路由。
- `access` 写在描述文件里，所以改它会改变指纹，需要重新批准。E4 的常驻进程按进程生命周期签发。

### 运行中的会话发现新工具

`crossagents mcp` 自己不缓存，每次 `tools/list` 都会重新问 hub。问题在另一头：它声明的是
`"capabilities": { "tools": {} }`，没有 `listChanged`，也没有主动通知，外部 agent 客户端开完会话后可能不再请求列表。

所以多加两个固定的内置工具，运行中的会话也能马上用上：

- `extension_tools`：列出当前可用的扩展工具和 schema。
- `extension_call(tool, input)`：调用其中一个。

新会话里，扩展工具直接出现在工具列表中。之后可以再试 `notifications/tools/list_changed`，但各家 CLI 是否
支持还没验证，不作为 v1 的依赖。

### 等待和取消

agent 和 GUI 的等待方式不同：

- **GUI**：Electron bridge 普通请求 30 秒就放弃（`REQUEST_TIMEOUT_MS`），而工具最长能跑 10 分钟，所以不能一直等一个请求。
  `POST /v1/extensions/{name}/tools/{tool}` 立刻回 `202` 和 `call_id`，结束时发 `extension.call.finished`，结果用
  `GET /v1/extensions/calls/{id}` 取（内存保留 10 分钟），`DELETE` 同一路径取消。
  `ca.tools.call` 把这些包成一个带 `cancel()` 的 Promise。它**从不自动重试**，因为工具可能有副作用。
- **agent**：`/v1/me/tools/{name}` 照旧同步等待。这条路径用 `tokio::spawn` 脱离了请求，客户端放弃后进程还会继续跑到超时，
  各家 CLI 自己的 MCP 超时见下。

**MCP 工具调用超时实测**（2026-11-05，本机 Windows；做法：一个只有 `wait(seconds)` 一个工具的 stdio MCP 服务，
让 CLI 非交互地调它，看 CLI 是否等到服务器答复）：

| CLI | 版本 | 70 秒 | 650 秒 | 结论 |
|---|---|---|---|---|
| CodeBuddy | 2.179.2 | 成功 | 成功（约 650 秒后拿到结果） | 超过工具 10 分钟上限也不超时 |
| Codex | 0.166.0-alpha.5.1（`~/.codex/plugins/.plugin-appserver/codex.exe`，非 PATH 上的发行版） | 成功 | **失败**：`timed out awaiting tools/call after 300s` | 超时在 70 到 300 秒之间不再细分，实测只到「300 秒时已放弃」；没有试 `tool_timeout_sec` 配置 |
| Claude Code | 2.1.317（Claude Desktop 带的那份，`%LOCALAPPDATA%/Claude-Data/*/claude-code/*/*/claude.exe`，不在 PATH 上；`claude -p --permission-mode bypassPermissions`） | 成功 | 成功（约 650 秒后拿到结果） | 超过工具 10 分钟上限也不超时 |

含义：Claude 和 CodeBuddy 不会先于 hub 的 10 分钟上限放弃；扩展工具要超过约 5 分钟，Codex 会话里的直接调用和 `extension_call` 都会在 CLI 一侧失败，而 hub 里的进程还会跑到
自己的超时（结果丢失）。长任务的作者应把 `timeout_secs` 压在 300 以内，或自己拆成「启动 + 轮询」。
`extension_api`（E3）要把这一条告诉作者。上面各数字只对测过的版本成立。

### 接口形状

E1（后端）和 E2（GUI）就靠这张表对接。类型都在
[crates/api-types/src/lib.rs](../crates/api-types/src/lib.rs)，TS 绑定在 `bindings/`，级别在
[crates/hub/src/access.rs](../crates/hub/src/access.rs)、口径在 [API-AUTH.md](API-AUTH.md) §3。

| 方法 | 路径 | 级别 | 请求 | 响应 |
|---|---|---|---|---|
| GET | `/v1/extensions` | read | — | `ExtensionsResponse { extensions: ExtensionInfo[] }` |
| GET | `/v1/extensions/{name}/logs` | read | — | `ExtensionLogsResponse { name, content, truncated }` |
| POST | `/v1/extensions/{name}/enable` | admin | `EnableExtensionRequest { fingerprint }` | `ExtensionInfo` |
| POST | `/v1/extensions/{name}/disable` | write | — | `ExtensionInfo` |
| POST | `/v1/extensions/{name}/auto-trust` | admin | `SetExtensionAutoTrustRequest { auto_trust }` | `ExtensionInfo` |
| GET | `/v1/extensions/{name}/pending/diff` | read | — | `ExtensionDiffResponse` |
| POST | `/v1/extensions/{name}/tools/{tool}` | write | `ExtensionCallRequest { input }` | `202` + `ExtensionCallAccepted { call_id, fingerprint, timeout_secs, … }` |
| GET | `/v1/extensions/calls/{id}` | read | — | `ExtensionCallResponse` |
| DELETE | `/v1/extensions/calls/{id}` | write | — | `ExtensionCallResponse`（`status: cancelled`） |

几个约定：

- **`ExtensionInfo` 是唯一的状态来源。** 启用与否、自动信任、当前指纹、上一指纹、待批准记录、以及源目录为什么
  不能用（`error`）都在里面，启用、停用、改开关都回这一份，省得客户端自己拼。`service` 是这次 hub 运行的
  实时状态（见下面「常驻进程」），其余字段重启后照旧。
- **一个扩展两份快照同时存在**：`current`（跑着的）和 `pending.version`（等批准的）。两者都是
  `ExtensionVersion`：指纹、`api`、`access`、工具表（含 `qualified_name`）、附加说明、事件钩子（`hooks`）、
  是否声明常驻进程（`service`）、GUI 入口、文件数和字节数。
  GUI 的批准界面需要的「这一版有什么」就是它；常驻进程的命令本身留在 `extension.toml` 里，差异视图会显示。
- **批准界面要看差异**，所以有 `GET /v1/extensions/{name}/pending/diff`：待批准快照对当前版本的 unified diff，
  加一份逐文件的增删统计（二进制文件只出现在统计里），再单独标出 `access` 有没有变——它决定扩展的 token 能干什么，
  不能混在一堆文件改动里让人滑过去。没有待批准记录就 404；第一个版本 `against` 为空，全是新增。
- **调用的身份不进请求体**：`ExtensionCallRequest` 只有 `input`，`caller` 由 hub 从凭据推出（见上文）。
- **调用绑定指纹**：`202` 回的 `fingerprint` 就是这次调用用的快照，中途切版本不会把它挪过去，
  `extension.call.finished` 和 `ExtensionCallResponse` 都带同一个指纹。
- **事件只报「变了」**，不带状态：`extension.changed { name }` 让客户端重读 `GET /v1/extensions`，
  和现有的 `plugin.changed` 一个路子；`extension.call.finished` 带 `call_id`、指纹、`status`、耗时和错误，
  结果仍然去 `GET /v1/extensions/calls/{id}` 取。

### 事件响应：`[[on]]`

每条 `[[on]]` 在匹配的事件发生时起一个进程。它**只观察，不拦截**：事件先落库、广播，然后才起进程，
进程的退出码和 stdout 都不改变事件，也不产生返回给发布者的结果。

- 描述文件：`event`、`command`、可选 `timeout_secs`（默认 60，范围 1..600）。到点用 Job Object 杀掉整个进程树。
- stdin 是一个 JSON：`{ "event": <完整事件>, "caller": { "kind": "event" } }`。`event` 就是 `Event` 本身
  （`seq`、`id`、`ts`、`schema`、`type`、`data`）。
- 工作目录、环境变量（`CROSSAGENTS_URL`、`CROSSAGENTS_TOKEN`、`CROSSAGENTS_EXTENSION_DATA`、hub 的代理变量）
  和工具完全一样；token 的级别取自 `access`，进程树结束就作废。
- stdout 和 stderr 都进 `<扩展名>.log`；**失败只记日志，不重试**。
- **只响应 hub 运行期间的新事件**：监听从订阅那一刻开始，重启后不补发，也不回读事件日志。
- 并发和工具**共用**同一组名额（每扩展 4、全局 16）。排满时这个事件被丢弃，并在日志里写一行原因，不排队。
- **停用**：运行中的钩子和工具调用一样，给 5 秒宽限后结束整棵进程树；token、快照租约和并发名额随之释放，
  不会拖到钩子自己的 `timeout_secs`。
- **不接受 `extension.*` 事件。** 描述文件里出现就直接判为无效（`extension_check` 和批准都会被拒），
  运行时再挡一次。这只挡住扩展之间的**直接**循环；经写接口的循环（比如 `access = "write"` 的钩子订阅
  `agent.updated`，又去 `POST /v1/agents/{id}/topic`）它挡不住，所以另有熔断。
- **限速熔断。** 每个扩展的钩子在 60 秒滑动窗口内最多启动 60 次；超过的事件直接丢弃，每个窗口只写一条
  日志说明触发了熔断。这个上限单独计算，和并发名额（4/16）无关：它既不占名额，也不因为名额有空而放宽。
- 同一个事件在一个扩展里只能有一条 `[[on]]`，重复写判为无效。

### 常驻进程：`[service]`

- 每个已启用扩展最多一个进程。`command` 写在描述文件里，工作目录是快照；`CROSSAGENTS_TOKEN` 按进程生命周期
  签发，进程树结束即作废（重启后换新 token）。
- stdin 关闭；stdout 和 stderr 全部写进 `<扩展名>.log`，不设 1 MB 上限（那是给工具结果的），日志按 10 MB 滚动。
- 异常退出按退避重启：1 秒起，每次翻倍，最长 60 秒；连续运行满 60 秒后重置为 1 秒，重启计数清零。
  退出码 0 视为「要求重启」，等 1 秒再起，不计入崩溃退避。
- **切换版本**：先停旧进程，再在新快照上起新进程。**停用**：和工具调用一样给 5 秒宽限，然后杀掉进程树。
  宽限按扩展各算各的，不同扩展的停用和版本切换并行进行，一个扩展等宽限不挡住另一个。
  **hub 退出**：全部杀掉（正在等停用宽限的也算），不跨 hub 存活；新 hub 启动时按当时的快照重新起。
- 常驻进程不占工具的并发名额（4/16 是给调用的）；它每扩展至多一个，本身就是上限。
- 运行状态通过 `ExtensionInfo.service` 报告：`state`（`stopped` / `starting` / `running` / `backoff`）、
  `restarts`（上次稳定运行 60 秒以来的重启次数）、`last_exit`（最近一次退出的原因，含启动失败）。
  它是这次 hub 运行的状态，不写进 `extensions.json`。

两项都在描述文件里，所以都计入指纹，必须由人批准那一版快照之后才运行：`ExtensionVersion.hooks` 和
`ExtensionVersion.service` 说的就是这一版声明了什么。

## 4. 附加说明

`[[prompts]]` 按 group 把文件内容追加到 hub 的说明之后。hub 的说明现在是 `protocol.rs`、`brief.rs` 里的字符串，
v1 只追加，不替换：替换后 hub 再改说明，覆盖的版本就会悄悄过时。追加已经够用来写团队约定和偏好。

实现：`collab.rs` 的 `prepare` 在拼好整段说明（身份、hub 的指南、规则、教训）之后，把已启用扩展里
`groups` 与会话所属 group 有交集的 `[[prompts]]` 文件内容按扩展名、再按描述文件里的顺序接在**最后**
（`protocol::with_extension_prompts`，空的什么都不加）。文件从已批准的快照读，不是源目录，所以改完要批准了才生效；
每个文件最多读 64 KB。`groups` 为空的条目不会被任何会话选中。

每次开新会话时读文件，所以批准后下个会话就生效。运行中的会话不受影响。

### 流式规则：`[[rules]]`

`[[rules]]` 让扩展像项目自己的 `.crossagents/rules/*.md` 一样提供流式规则（文件格式见
[tasks/STREAM-RULES.md](tasks/STREAM-RULES.md)）：规则平时不进上下文，agent 的输出或工具调用命中 `match`
时，才在它正在跑的这一轮里送进去。每条规则带 `id`、`match`（正则）和 `on`（`output` / `tool` / `both`）。

和 `[[prompts]]` 一样，文件从已批准的快照读，不读源目录，所以改完要批准了才生效；每个文件最多 64 KB，
路径必须是快照里的普通文件（写错、指向目录外，启用时就拒绝）。规则 id 前面加上扩展名
（`standup/no-sleep`），两个扩展可以有同名规则。文件里 frontmatter 坏了、正则编译不过，只记 warn 并跳过
这一条，和项目规则一样，不影响会话。

实现：`extensions/rules.rs` 的 `ExtensionRuleFiles` 实现 `stream_rules::RuleSource`；
`stream_rules::load` 在会话开始时把项目规则和 `enabled_snapshots` 找到的已启用快照里的 `[[rules]]`
一起读进来，id 重复的后者跳过。改 `[[rules]]` 不影响已经在跑的会话，下个会话才生效。

## 5. GUI

### 加载

- Electron 主进程注册 `crossagents-ext://<name>/<指纹>/<path>`，映射到对应快照（解析后校验路径，防止跳出目录），
  只响应当前版本和上一个版本。主程序目前没有自定义协议：要用 `registerSchemesAsPrivileged` 声明为 standard、secure，
  并让响应带上 CORS 头（页面来自 `file://`，模块脚本要过 CORS）。
- **CSP 要开的不只是脚本。** [apps/gui/index.html](../apps/gui/index.html) 现在是
  `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; …`，
  面板会带自己的 CSS、图标和字体，所以 `script-src`、`style-src`、`img-src`、`font-src` 都要加
  `crossagents-ext:`（`font-src` 现在没单列，靠 `default-src`，要补一条）。`connect-src` 不加：扩展访问 hub
  走 bridge，不自己发请求。开发服务器的 `devCsp`（`vite.config.ts`）同步改。隔离文档自己的 CSP 见 §9「Webview」。
- **指纹放在路径里，不用查询参数。** `index.js?v=2` 里的 `import "./panel.js"` 解析出的 URL 没变，依赖可能还是旧的；
  路径带指纹后，相对 import、CSS 和图片都自然落到同一版本。资源用 `new URL("./x.png", import.meta.url)` 取。
- 切换版本时：宿主先清理旧版本的注册，再 `import()` 新入口，页面不用刷新。旧模块留在内存里直到整页刷新，模块缓存无法清除，
  频繁调试时有轻微的累积，可以接受。
- 改动通知沿用现有的 `plugin.changed` 思路：**hub 每 2 秒轮询一遍源目录的文件元数据**（路径、大小、mtime），
  发现和上次不一样才去复制快照、算指纹、发 `extension.changed`。不引入 `notify` 依赖：扩展目录就几十个文件，
  2 秒的延迟在「改一下看一下」的节奏里感觉不出来，而文件监视在 Windows 上的重命名和编辑器临时文件要专门绕，
  省下的那点延迟不值得。元数据一样就不算变化，也就不会无谓地重算指纹。

### 接口

```js
// gui/index.js：不需要构建，也不需要 JSX
export function activate(ca) {
  const { html, useState } = ca;
  ca.tabs.register({
    id: "standup",
    label: "站会",
    render: ({ follow }) => html`<div>...</div>`,
  });
  // 不需要返回清理函数：通过 ca 注册的东西由宿主记录和清理。
  // 只有自己开的资源（定时器、DOM 监听）才需要返回一个函数。
}
```

| 接口 | 用途 |
|---|---|
| `ca.tabs.register` | 工具列的标签页 |
| `ca.pages.register` | 主区的一个页面，加侧栏入口 |
| `ca.toolCards.register(toolName, render)` | 自定义某个工具调用在对话里的卡片 |
| `ca.settings.register` | 插件自己的设置区块 |
| `ca.styles.add(path)` | 加一份 CSS；颜色用现有的 CSS 变量，换主题自动跟随 |
| `ca.hub.request` / `ca.events.on` | 走现有 bridge 调 hub、订阅事件 |
| `ca.tools.call(tool, input)` | 调自己扩展的工具，让界面和后端能接起来 |
| `ca.React`、`ca.html`、`useState` 等 | 和主程序共用同一个 React；`html` 是 [htm](https://github.com/developit/htm)，不用构建 |

扩展只能拿到 `ca`，import 不到 `src/` 里的模块。这样内部怎么重构都不会弄坏扩展，`api = 1` 才有意义。

### 要改的地方

- `panes.ts`：`TabId` 加 `ext:<name>/<id>`，写法同现有的 `term:`。**注意**：`load` 现在会丢掉不认识的标签，
  而扩展是异步加载的，必须等扩展列表到齐再判断，否则每次启动都会把扩展的标签页弄丢。
- `ToolColumn.tsx` `PaneBody`：加一个分支，渲染注册的标签页。
- `electron/allowed.cts`：把 §3「接口形状」那张表里的九条逐条登记（`allowed.test.cts` 会从 `src/api.ts`
  反查，漏登记就失败）。扩展调用的其余接口仍受这张表限制。
- `apps/gui/package.json`：**加 `htm` 依赖**。`ca.html` 要给扩展一个不用构建就能写模板的入口，主程序现在只有
  `react`，没有 htm。它是运行时依赖（放 `dependencies`），由宿主 import 后通过 `ca` 传给扩展，扩展自己不装包。
- hub：§3「接口形状」的九条路由，级别写进 [crates/hub/src/access.rs](../crates/hub/src/access.rs)，
  口径写进 [API-AUTH.md](API-AUTH.md)。

### 能隔离什么，不能隔离什么

扩展和主程序同一页面，所以**不保证**一个扩展坏了只影响它自己。能做到的是隔离常见的错误，整体故障靠安全模式恢复：

| 情况 | 结果 |
|---|---|
| 组件渲染时抛错 | 每个注册的组件包一层 ErrorBoundary，这一块显示错误和「停用」按钮 |
| `ca.events.on` 等回调抛错 | 宿主包一层 try/catch，记到该扩展的日志 |
| `activate` 抛错或异步超时 | 标记为失败，清理它已注册的内容，其他扩展照常加载 |
| 自己绑的 DOM 事件、裸 Promise 出错 | ErrorBoundary 接不住，也无法可靠归到某个扩展，只进控制台 |
| 死循环、把内存吃光 | 整个页面卡死，计时器也不会运行，只能走安全模式 |
| CSS 写坏整个页面 | 只能走安全模式 |

**注册由宿主负责清理。** 每个扩展拿到自己的 `ca`，宿主记下它注册的标签、订阅和样式。这样 `activate` 注册到一半抛错，
拿不到返回值，也不会留下残留。

**安全模式的入口**，页面卡死时也要能用，所以都放在主进程：

- 启动参数 `--safe-mode`，不加载任何扩展。
- 托盘菜单「以安全模式重启」。
- 窗口 `unresponsive` 事件触发时弹窗询问。
- 上次启动在加载扩展时没能走到就绪，下次自动进入安全模式并说明原因。

## 6. 信任模型

先说清楚**不能防什么**：扩展进程以用户身份运行，读得到 `~/.crossagents/token`；GUI 扩展和主程序同一页面，
能调终端接口。和主页面不同源的 webview 见 §9。`access` 级别只防写错，防不住恶意代码。bypass 模式的 worker 本来就能动这些文件。
也就是说，这里没有沙箱，和 dsh 的已安装插件一样（dsh 也在宿主进程里跑）。

能做的是不让扩展**悄悄生效**，防误操作和提示词注入：

- agent 可以随便写源目录，但 hub 只执行已批准的快照，没批准的代码一行都不会跑。
- **源文件变了，旧版本继续运行**，因为跑的是快照。GUI 提示「有更新，查看并启用」，批准后才切换。
- **启用必须由人批准**，GUI 显示这一版的工具、说明、面板和 `access`，以及和当前版本的差异。
  这一条定了：不改成「lead 可批」。hub 别处尽量少设限，是因为那些限制挡的是模型做错事；这里挡的是
  **让一段没人看过的代码以本机用户身份跑起来**，而写这段代码的和要批准它的会是同一个模型。
  这不是能力问题，是提示词注入的入口：issue、网页、别的 agent 的消息都能变成「写个扩展并启用」。
  一次点击的代价很小，省掉它换来的是整条链路上没有任何人看过要执行什么。
- 调试时可以**按扩展**打开「自动信任更新」：源目录一变就生成新快照并切换，否则改一次点一次，就不算随手了。
  范围只限这一个扩展，没有全局开关——全局开关一开就等于回到「悄悄生效」。
- **两个开关都只有人能操作。** 启用和自动信任是 admin 级（[access.rs](../crates/hub/src/access.rs)），
  agent 的会话 token 只到 `/v1/me*`，到不了 `/v1/extensions*`；创造模式那组工具里也**没有**
  「启用」或「打开自动信任」，只有 `extension_request_enable`（发起待批准记录）。
  agent 能做的最多是让这条记录出现在人面前。

### 启用状态存在哪

落在 `<home>/extensions.json`，hub 重启后保留。写法照
[crates/hub/src/models.rs](../crates/hub/src/models.rs)（`ModelChoice`）：启动时读一次、读不出来就当空、
内存里一份 `RwLock`、每次改动先写 `.tmp` 再 `rename` 覆盖。**不做数据库迁移**——这是客户端侧的一点配置，
和事件日志、任务状态机不一样，没有跨步骤的不变量要维护，塞进库里只会多一次 schema 变更。

每个扩展存四样：

| 字段 | 作用 |
|---|---|
| 当前指纹 | 调用和 GUI 用哪份快照；没有就是从没批准过 |
| 上一指纹 | 旧快照留着，让切版本时还在跑的调用和已经 import 的 GUI 模块仍能解析 |
| 自动信任 | 这个扩展的源目录一变就自动切 |
| 待批准记录 | `(指纹, 发起时间, 发起者)`；没有超时，重启也还在 |

和 `ExtensionInfo` 一一对应（`current` 由指纹指向的快照读出），所以 `GET /v1/extensions` 不需要别的来源。
唯一的例外是 `service`：它是本次 hub 运行的进程状态，重启后从零开始。快照本身在 `extension-versions/`，
JSON 里只存指纹；指纹指向的快照不在了，就当没批准过，并在 `error` 里说明。

### 批准不走普通提问

GUI 可以复用提问的样式，但判定逻辑要独立。`human_ask`（`crates/orchestrator/src/human.rs`）按 run 策略可能直接拒绝，也可能超时后
采用默认答案，甚至默认取第一个选项，这和“必须由人点一次”冲突。

- `extension_request_enable(name)` 由 hub 生成快照、对快照算指纹，创建一条待批准记录，绑定 `(扩展名, 指纹)`。
- **hub 的轮询也会创建待批准记录**，不只靠这个工具：源目录变了（包括一个从没启用过的扩展第一次出现）就记一条，
  这样人自己写的扩展不必先让 agent 发起一次。`ExtensionPending.requested_by` 因此是可空的——轮询发现的没有发起者。
- 批准走 `POST /v1/extensions/{name}/enable`，请求体带指纹，级别是 admin。agent 的会话 token 到不了这个路由。
- **没有超时批准**，也没有默认答案。待批准记录可以一直挂着，hub 重启也还在（存在 `extensions.json` 里）。
- 源目录再变化，这条记录就被新指纹的记录顶掉，之前看过的那一版不能再批。
- 请求的指纹和记录不一致就 409，防止看的是一个版本、批的是另一个。
- 批准界面的差异来自 `GET /v1/extensions/{name}/pending/diff`（§3），`access` 变化单独标出。

## 7. 创造模式

不是单独的模式，只是一组工具加一份说明，任何 agent 都能用：

- `extension_api`：返回当前的接口版本、事件类型（并写明 `extension.*` 不能用于 `[[on]]`）、group、
  事件钩子和常驻进程的约定、GUI 插槽和一份完整示例。相当于 dsh 的 `cordis_inspect_*`。
- `extension_check(name)`：静态校验描述文件、schema 和入口文件，返回源目录的 `ExtensionVersion`；校验失败返回工具错误，不写启用状态。
- `extension_request_enable(name)`：由 hub 生成快照和指纹，返回 `ExtensionInfo`，只建立待批准记录，不启用、不改自动信任。
- `extension_logs(name)`：与 GET 日志路由返回同一份 `ExtensionLogsResponse`：当前日志末尾 64 KiB（UTF-8 容错解码），`truncated` 表示省略了更早字节；未调用过时内容为空，不包含轮转文件。

**不提供试跑工具。** hub 不执行未批准的代码，这条规则不留例外。agent 本来就有 shell，写完可以自己按协议喂 stdin 跑一遍，
这是以它自己的身份和权限在跑，不经过 hub，也拿不到扩展 token。`extension_api` 的示例里写明这种做法。
`extension_check` 也只做静态检查，不执行任何扩展代码。

GUI 上一个入口「让 agent 做一个插件」：开一个新对话，把请求填进草稿，**不自动发送**（dsh 也是这样）。

## 8. 分阶段

| 阶段 | 内容 | 验收 |
|---|---|---|
| **E1 后端**（已完成，测试在 `crates/hub/tests/extensions_runner.rs`、`extensions_registry.rs`） | 目录扫描、描述文件校验、快照与指纹、批准流程、一次性工具与运行限制、统一权限检查、扩展 token、`extension_tools/call`、异步调用接口、附加说明 | 手写一个工具，不重启 hub，运行中的会话能调用；改文件后旧版本继续跑，批准后下一次调用用新版本；超时、超并发、无权限、输入不合法、未批准都有测试；hub 重启后启用状态和待批准记录都还在；实测各家 CLI 的 MCP 超时 |
| **E2 GUI**（已完成） | 自定义协议、`ca` 接口与注册清理、标签页、工具卡片、错误隔离、安全模式 | 手写一个带相对 import 的面板，改依赖文件后不刷新页面就更新；渲染出错只坏这一块；`activate` 半路抛错没有残留；死循环后能进安全模式；45 秒的工具在界面上正常返回 |
| **E3 创造流程**（已完成） | `extension_api`、检查、请求启用、GUI 入口和日志 | **在对话里说一句“做一个 X”，不编译、不重启，几分钟内能用上带面板的工具** |
| **E4 事件与常驻**（已完成，测试在 `crates/hub/tests/extensions_hooks.rs`、`extensions_service.rs`） | `[[on]]`、`[service]` 的描述文件字段与校验；`Runner::start_command` / `Runner::issue_token`；`Registry::on_version_changed`；`hooks.rs` / `service.rs` 的公开骨架，启动与关闭已接进 `lib.rs`；钩子的限速熔断与停用回收；服务按扩展并行协调、关闭时打断宽限 | 约定见 §3「事件响应」「常驻进程」。手写一个扩展：事件到达时起进程且不拦事件、失败只进日志、重启不补发；写接口形成的循环被熔断压在 60 次/分钟内；停用 5 秒后钩子和常驻进程都结束、token 作废；常驻进程崩溃后按退避重启，换版本停旧起新，两个扩展的宽限并行，hub 在宽限中途退出也不残留 |

E1 和 E2 涉及的文件不重叠，可以并行：`/v1/extensions` 的形状已经定在 §3「接口形状」，
类型和事件已在 `crates/api-types`、`crates/core/src/event.rs` 落地，TS 绑定已生成，两边照着写就行。

**v1 不做**：拦截核心流程的钩子（超时和失败时算通过还是拒绝，要先想清楚）、市场和安装源、npm 依赖管理、
项目级扩展（`<repo>/.crossagents/extensions`，clone 一个仓库就带上可执行代码，信任问题要另外想）、多节点。
其中「多节点」的后续在 §9：`runs_on = "node"`、字节走 hub，同盘假设记为 A2 / B3 的已知限制；细节在
[MULTI-NODE.md](MULTI-NODE.md) §12 和 §16「扩展在多节点下」。

## 9. 扩展 api 2

**只有设计，W1–W4 未开工。** 目标是 VS Code 那种可扩展性：扩展逻辑仍在进程外（现有的工具、`[[on]]`、`[service]`，
相当于 VS Code 的 extension host，这一层不动），丰富界面放进隔离的 webview。不推翻现有生命周期：快照、指纹、
人工批准、扩展 token 都照旧。

### 现在为什么做不到

现在做不了 VS Code 那种隔离界面：主页面嵌不了别的文档，二进制接口不给字节，
页内扩展用的是人的总 token，hub 没有「请 GUI 执行再交回结果」的通道。
`api = 2` 的描述文件今天会被拒绝。

- **主页面 CSP 嵌不了 iframe，也跑不了 WASM。** [apps/gui/index.html](../apps/gui/index.html) 现在是
  `default-src 'self'; script-src 'self' crossagents-ext:; style-src 'self' crossagents-ext: 'unsafe-inline'; img-src 'self' crossagents-ext: data:; font-src 'self' crossagents-ext: data:; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'`。
  没有 `frame-src`，回落到 `default-src 'self'`，嵌不了 `crossagents-ext:` 的 iframe。`connect-src` 只有 `'self'`，
  扩展连自己快照里的文件都 fetch 不到。没有 `'wasm-unsafe-eval'`，也没有 `blob:`，WASM 解码（three.js 的 Draco / KTX2）、
  Worker、blob 贴图都会被拦。[apps/gui/vite.config.ts](../apps/gui/vite.config.ts) 的 `devCsp` 只给 `script-src` 加
  `'unsafe-inline'`、给 `connect-src` 加 `ws://localhost:* ws://127.0.0.1:*`，同样没有 `frame-src`。
- **所有 webContents 都不许开窗、不许离开应用、不许 `<webview>`。** [apps/gui/electron/main.cts](../apps/gui/electron/main.cts)
  的 `web-contents-created`：`setWindowOpenHandler` 一律 deny（http(s) 交给系统浏览器），`will-navigate` 拦住离开应用，
  `will-attach-webview` 拒绝。
- **二进制文件不给字节。** [crates/api-types/src/lib.rs](../crates/api-types/src/lib.rs) 的 `FileContentResponse`：
  二进制没有 `text`，只有 `binary: true`，响应里没有字节。
- **页内 `ca.hub.request` 用人的凭据。** [apps/gui/src/extensions/host.ts](../apps/gui/src/extensions/host.ts) 的 `makeCa`
  把 `hub.request` 原样转给 `options.request`。页面上接的是 [apps/gui/src/extensions/defaults.ts](../apps/gui/src/extensions/defaults.ts)
  的 `hubRequest`，走主进程 bridge，带的是 `<home>/token` 里的总 token。GUI 扩展声明的 `access` 因此实际不生效。
  §6 已经承认页内扩展和主程序同一页面。
- **hub 不会请 GUI 做事再收回结果。** hub 到 GUI 的方向是事件流：客户端来订阅，hub 只往下推。扩展相关的事件是
  `extension.changed` 和 `extension.call.finished`（[crates/core/src/event.rs](../crates/core/src/event.rs)）。
  没有一条「请 GUI 执行一件事、把结果交回来」的通道。
- **`api = 2` 今天会被拒绝；静默丢掉的是 `api = 1` 里的未知字段。**
  [crates/hub/src/extensions/manifest.rs](../crates/hub/src/extensions/manifest.rs) 的 `Manifest::read` 只认 `api == 1`，
  否则报 `unsupported extension api {}; expected 1`。旧 hub 遇到 `api = 2` 的描述文件是明确拒绝。
  `Manifest` 和 `Tool` 都没有 `deny_unknown_fields`，所以写进 `api = 1` 描述文件的新表和新字段会被丢掉，hub 不报错。
  因此新字段只在 `api = 2` 下有效，新 hub 同时接受 1 和 2。

### Webview

隔离文档就是 iframe，地址 `crossagents-ext://<name>/<指纹>/<entry>.html`。不用 `<webview>` 标签（Electron 不推荐，
现在也被 `will-attach-webview` 拦住），不用 WebContentsView（原生叠层，和 React 布局、弹出菜单配合差）。

协议已经按这个来源注册好。[apps/gui/electron/main.cts](../apps/gui/electron/main.cts) 的 `registerSchemesAsPrivileged` 把 `crossagents-ext` 标成 standard、secure、
supportFetchAPI、corsEnabled。standard 之后来源是 `crossagents-ext://<name>`：每个扩展一个来源，与主页面、与其他扩展都不同源。
[apps/gui/electron/extProtocol.cts](../apps/gui/electron/extProtocol.cts) 的 `extensionFile` 已经按这个形状解析 URL：
`crossagents-ext://<name>/<指纹>/<路径>`。子 frame 不跑 preload，拿不到 `window.crossagents`。窗口现在的 `webPreferences`
与此一致：`sandbox: true`，没有 `nodeIntegrationInSubFrames`（默认 false，preload 不进子 frame）；preload 在
[apps/gui/electron/preload.cts](../apps/gui/electron/preload.cts)。

每个文档的 CSP 由 `serveExtension`（`main.cts`）按已批准的 `[capabilities]` 生成：默认 `default-src 'none'`，只许自己的来源；
声明 `wasm` 才加 `'wasm-unsafe-eval'`；`net` 里的来源才进 `connect-src`。主页面 CSP 只加 `frame-src crossagents-ext:`，
`devCsp` 同步。`serveExtension` 现在回 `Access-Control-Allow-Origin: *`，收紧到主程序和同一扩展。

子 frame 的导航用 `will-frame-navigate` 锁在自己的来源。`net` 再在 `session.webRequest` 拦一道，不只靠 CSP。

通信靠宿主转交的 MessagePort。webview 里的 `ca` 是精简的：`tools.call`（只能调自己扩展的工具；§3 里这只是约定，
到 webview 才是真边界）、`events.on`、`hub.request`（用 hub 给该扩展签发的 token，由 hub 按 `access` 检查）、
`state.get` / `state.set`、主题变量、`openFile`、`platform`。

隐藏时默认销毁，`retain = true` 才保留，靠 `state` 恢复。跨源 iframe 应跑在独立进程，死循环不拖垮主界面：**要实测**。

### 查看器

按文件类型决定怎么打开，相当于 VS Code 的 custom editor。描述文件写 `[[viewers]]`，`patterns = ["*.glb", "*.gltf", ...]`。

所有文件链接都走 [apps/gui/src/FileViewer.tsx](../apps/gui/src/FileViewer.tsx) 的 `openFile`。它先查匹配的查看器，
没有就照旧用文本查看器。现在的 `openFile` 没有查查看器，直接打开文本查看器；同文件里 `binary: true` 只显示
「这是一个二进制文件」，并建议用默认程序打开。

打开时宿主发一个句柄，webview 只能读这个文件所在目录（如 `crossagents-file://<句柄>/…`），这样 glTF 相对引用的
`.bin` 和贴图能加载。

**字节一律从 hub 的二进制接口流式取**，不走 `FileContentResponse` 那种 JSON，也不让 GUI 主进程读本机磁盘：
GUI 和 hub、节点可能不在同一台机器。下面两处是同盘假设，记为 [MULTI-NODE.md](MULTI-NODE.md) §12 的 A2（远程 GUI）
和 B3（远程开发）要改的已知限制；扩展在多节点下怎么拿字节，见同文 §16「扩展在多节点下」。

- `serveExtension`（[apps/gui/electron/main.cts](../apps/gui/electron/main.cts)）问过 hub 的 `/v1/extensions` 之后，用
  [apps/gui/electron/extProtocol.cts](../apps/gui/electron/extProtocol.cts) 的 `extensionRealFile` 读本机 `<home>/extension-versions`。
- `shell:open-path`（`main.cts`）经 hub 的 `files/resolve` 拿到 `abs_path`，再 `shell.openPath` 打开本机绝对路径。

### 浏览器视图

`kind = "browser"` 用 WebContentsView，不用 iframe。大站多用 `X-Frame-Options` / `frame-ancestors` 禁止被嵌；
SameSite 默认 Lax，跨站 iframe 不带登录 cookie。

每个扩展一个持久分区 `persist:ext-<name>`，这个分区里不注册 `crossagents-ext:`。规则单独一套：导航受 `net` 限制，
弹窗和 OAuth 开成新标签，摄像头、定位、通知交给人决定，下载要问，证书错误拒绝。现在默认会话的权限由
[apps/gui/electron/permissions.cts](../apps/gui/electron/permissions.cts) 的 `permissionAllowed` 决定，只放行
`clipboard-sanitized-write`，摄像头和麦克风在内的其余权限一律拒绝。浏览器分区不沿用这一条，改由人决定。
摄像头和麦克风在 macOS 上还要在打包时声明用途，见 [docs/tasks/MACOS.md](tasks/MACOS.md) §3「打包与分发」。

全局的 `web-contents-created` 改成只管主窗口和 webview。浏览器视图不走「不许开窗、不许离开应用、不许 `<webview>`」那三条。

叠层要同步位置（停靠栏拖拽、`setZoomLevel` 缩放），菜单和弹窗出现时隐藏，或换成截图。第一版可以先做成独立窗口。

控制接口由宿主经 `webContents.debugger` 实现：`navigate`、`snapshot`（可读文本加元素编号）、`click`、`type`、`scroll`、
`screenshot`。不把原始 CDP 交给扩展。网页内容是不可信输入，是提示词注入的入口，操作按站点批准。

### 代码在哪跑：`runs_on`

工具级字段，`runs_on = "hub" | "node" | "gui"`。

- **`hub`**：现在的行为，只用 hub 接口的工具。`[[on]]` 和 `[service]` 第一版都留在 hub。
- **`node`**：在调用它的 agent 所在节点上跑。阶段见 [MULTI-NODE.md](MULTI-NODE.md) §12，扩展在节点上的安排见同文
  §16「扩展在多节点下」。
- **`gui`**：运行器不起进程，发一个事件（如 `extension.gui_call`）。GUI 经两条新路由认领、交回结果；这两条路由只认人的凭据。
  沿用 `call_id`、超时和 `extension.call.finished`。没有 GUI 能执行就明确报错。多个 GUI 在线时交给人正在用的那个
  （打开着这个会话、最近有操作），不是先到先得。

agent 在节点上起的 `localhost:5173`，在另一台机器的 GUI 里打不开。优先用同机的 GUI，或经 node 转发端口。
连通方式见 [MULTI-NODE.md](MULTI-NODE.md) §14「连通方式」，扩展侧见 §16。

### 描述文件

入口是声明式的，全部计入指纹。GUI 不执行扩展代码就知道有哪些入口，用到时才加载，批准界面也能列出来。

- `[[views]]`：`kind = webview | browser`、`slot = page | tab`、`entry`、`retain`。
- `[[viewers]]`：按 `patterns` 匹配文件。
- `[capabilities]`：`net`、`wasm`、`files`、`browser` 等。

`ExtensionVersion`（[crates/api-types/src/lib.rs](../crates/api-types/src/lib.rs)）带上 capabilities、views、viewers。
批准差异里单独标出 capabilities 的变化，照 `ExtensionDiffResponse` 的 `access_changed`：它决定文档能连哪里、能不能跑 WASM，
要单独给人看。

`runs_on = "node"` 的工具允许按系统分别写命令。写法照 [DESIGN.md](DESIGN.md) §9.3：`[locate.paths]` 可以按
`windows`、`macos`、`linux` 分键。仓库里 `[locate]` 已经写成这种键的，只有
[crates/plugins/agents/codex.toml](../crates/plugins/agents/codex.toml) 的 `[locate.bundled]`（`windows` 和 `macos`）。
[crates/plugins/agents/claude.toml](../crates/plugins/agents/claude.toml) 是另一条：`[[install.methods]]` 写
`os = ["windows"]` 和 `os = ["macos", "linux"]`。[crates/plugins/agents/cursor.toml](../crates/plugins/agents/cursor.toml)
只有 Windows 的安装方法（`os = ["windows"]`）。`extension_check` 对明显只属于一个系统的命令给提示。

页内 `ca` 保留，定位成「小而原生」的一档：卡片、设置、小标签。要不要逐步收紧到按 `access` 检查：**待定**。
现有 [extensions/debug/extension.toml](../extensions/debug/extension.toml) 的 `access = "read"`，收紧不会弄坏它。

### 不做

放宽主页面 CSP、开 `<webview>`、把 bridge（`window.crossagents`）给子 frame、把原始 CDP 交给扩展、npm 依赖管理。
主页面 CSP 只加 `frame-src crossagents-ext:`。npm 依赖管理在 §8 的 v1 不做里已经有，api 2 也不做。

### 分期

写成和 §8 一样的三列表（阶段 / 内容 / 验收）。四个阶段都还没开工。

| 阶段 | 内容 | 验收 |
|---|---|---|
| **W1**（未开工） | webview、`[capabilities]`、`api = 2`、批准界面 | three.js 页面带 WASM 正常渲染；webview 里写死循环，主界面仍能用；停用后 webview 消失 |
| **W2**（未开工） | 查看器，以及限定范围的文件访问（字节走 hub） | 点对话里的 `out/car.gltf` 打开 3D，相对 `.bin` 和贴图能加载；范围外的路径被拒 |
| **W3**（未开工） | 浏览器视图 | 登录 GitHub 后重启仍是登录状态；禁止被嵌的网站能打开；菜单不被盖住 |
| **W4**（未开工） | GUI 侧工具 | agent 能打开网页、读成文本、点击、截图，人看得到、能接手；没有 GUI 时明确报错 |

这里的 W1–W4 是扩展 api 2 的实现阶段，和 [docs/tasks/W1.md](tasks/W1.md) 的轻量 Work 不是一回事。
