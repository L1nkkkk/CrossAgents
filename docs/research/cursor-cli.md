# cursor-cli 记录

2026-11-02，对接 `cursor-native` 驱动前的协议梳理。**本文来自设计确认，不是本机实测**：写这份记录时本机没装 Cursor CLI，命令、事件名与参数以官方文档和设计讨论为准，未逐条跑过；下文标「已确认」的是设计阶段定下的结论，装好 CLI 后仍需按 [with-cli.md](with-cli.md) 的方式补一次实测（尤其 `--stream-partial-output` 的去重规则和 MCP 挂载）。

## 结论

走 `agent -p --output-format stream-json --stream-partial-output`，每轮一个进程，会话靠 `--resume <chatId>` 在 Cursor 侧续上。输出是 NDJSON，一行一个事件，每行都带 `session_id`。驱动能拿到的比 Claude 侧少：没有逐次权限询问（只有 `--force`/`--yolo` 与 `--sandbox` 两档），没有 `--system-prompt`/`--user-rules`（开场说明得前置进第一条 prompt），没有 `--mcp-config`（MCP 只能从配置文件或临时 plugin 读），没有 cancel 子命令（打断只能杀进程），stream-json 也不带 token 数（只有 `duration_ms`）。因此能力表里 `permission_callback` 与 `usage` 都只能降级。

## 命令行

| 参数 | 作用 |
|---|---|
| `-p` | 非交互，读 prompt、打印结果后退出 |
| `--output-format stream-json` | NDJSON 事件流，一行一个 JSON |
| `--stream-partial-output` | 文本增量（见下节，按 `timestamp_ms` / `model_call_id` 的有无区分增量与重复） |
| `--resume <chatId>` | 接着某个 chat 跑，上下文延续 |
| `--force` / `--yolo` | 放行工具调用，不再询问 |
| `--sandbox` | 沙箱内执行 |
| `--plugin-dir <dir>` | 临时挂一个 plugin（`plugin.json` + `mcp.json`），配合 `--approve-mcps` |
| `--approve-mcps` | 预先批准 plugin 带来的 MCP server，否则不会挂上 |

没有的：`--mcp-config`、`--system-prompt`、`--user-rules`。也没有 `agent cancel` 之类的子命令。

## stream-json 的输出

每行一个 JSON 对象，都有 `session_id` 字段，所以单看任意一行都能知道它属于哪个会话。

| 事件 | 内容 |
|---|---|
| `system`（`subtype: "init"`） | 开场信息：`cwd`、`model`、`session_id` |
| `assistant` | 回复文本，在 `content[].text` |
| `tool_call`（`started` / `completed`） | 工具调用。种类有 `readToolCall`、`writeToolCall`、`function`；`started` 带 `args`，`completed` 带 `result` |
| `result` | 一轮的收尾：`subtype: "success"`、`is_error`、`result`（最终文本）、`session_id` |

`result` 是判断一轮成败的唯一依据：`is_error` 为真或 `subtype` 不是 `success` 就是失败，此时不一定有 `assistant` 文本。

### `--stream-partial-output` 的去重

开了增量之后，`assistant` 事件会以三类形态反复出现，按官方文档（`cursor.com/docs/cli/reference/output-format`）的表格区分：

| `timestamp_ms` | `model_call_id` | 是什么 | 处理 |
|---|---|---|---|
| 有 | 无 | 流式增量，含新文本 | 取，追加 `message.content[].text` |
| 有 | 有 | 工具调用前的缓冲 flush，与已收到的文本重复 | 跳过 |
| 无 | 无 | 轮末的最终 flush，同样是重复 | 跳过 |

即**只有带 `timestamp_ms` 且不带 `model_call_id` 的才是新文本**。驱动按这两个字段的有无过滤，不要按 `content` 内容去重，也不要用末尾 `result.result` 再补一遍文本（它等于全部增量拼接，会重复）。

## 会话

- `--resume <chatId>` 续会话。`chatId` 就是事件里的 `session_id`。
- 新会话想先拿到 id：`agent create-chat` 建一个空 chat，返回 chat id，之后每轮都用 `--resume` 接着跑。这样驱动可以在开会话时就确定 id 并上报，不必等第一轮结束。
- 会话由 Cursor 侧按 chat id 保存，hub 不存对话内容；hub 重启时正在跑的那一轮丢失，会话本身可以续。

## MCP

没有 `--mcp-config`，所以不能像 Claude/Codex 那样一次性传一份 MCP 配置。两条路：

- 配置文件：`.cursor/mcp.json`（项目级）或 `~/.cursor/mcp.json`（用户级）。
- `--plugin-dir <dir>` 指向一个临时 plugin 目录，里面放 `plugin.json` 与 `mcp.json`，再带 `--approve-mcps`。协作工具（`crossagents mcp`，带会话令牌）走这条：每个会话生成一个临时 plugin 目录，用完即弃，不去改用户的全局配置。

## 开场说明与权限

- 没有 `--system-prompt` / `--user-rules`，开场说明只能拼在第一条 prompt 前面。后续轮次靠会话上下文保留，不重复拼。
- 没有逐次权限询问。工具调用要么放行（`--force` / `--yolo`），要么在沙箱里跑（`--sandbox`），没有中间档，因此驱动的 `permission_callback` 能力只能报不支持。
- 打断：没有 cancel 子命令，`interrupt()` 只能杀进程，按 `killed` 上报退出原因。
- 用量：stream-json 不带 token 数，只有 `duration_ms`，`usage` 能力只能报不支持（或只给时长）。
