# with-cli 实测记录

2026-10-26，with-cli v0.31.0（Windows），对接 `with-native` 驱动前的实测。

## 结论

走 `with-cli chat -o stream-json`，每轮一个进程，会话靠 `--sessionId` 在 With 服务端续上。不走 `with-cli acp`：ACP 握手、模型和配置项都正常，但工具调用一律返回"工具执行失败"。换过 agent/manual 模式、`access_mode`、客户端 fs/terminal 能力声明、工作目录，结果都一样；同样的任务用 `chat` 跑是正常的。另外 manual 模式下它也不发 `session/request_permission`，而是直接失败。

## chat 的输出

每行一个 JSON：`{"type": ..., "timestamp": ..., "rawEvent": {...}, "offset": n}`。`rawEvent.message_id` 就是本轮的 request id，`conversation_id` 就是会话 id。

| type | 内容 |
|---|---|
| `RUN_STARTED` / `RUN_FINISHED` | 一轮的开始和正常结束 |
| `RUN_ERROR` | 一轮失败 |
| `STEP_STARTED` / `STEP_FINISHED` | 一次模型调用；`STEP_FINISHED.token_usage` 给出 `prompt_tokens`、`completion_tokens`、`total_tokens`、`prompt_tokens_details.cached_tokens`、`completion_tokens_details.reasoning_tokens` |
| `TEXT_MESSAGE_START/CONTENT/END` | 回复文本，增量在 `content` |
| `THINKING_TEXT_MESSAGE_START/CONTENT/END` | 思考文本（`--enable-thinking`） |
| `TOOL_CALL_START` | `tool_call_id`、`name`、`ch_name`；`default_show: false` 表示内部簿记（如 `task_planning`"任务规划中"） |
| `TOOL_CALL_ARGS` | 参数：整份放在 `document`，或者增量放在 `patchs`（`{op, path, value}`，对字符串做 `add` 就是往后拼接） |
| `TOOL_CALL_END` | 参数已经完整 |
| `TOOL_CALL_RESULT` | `rawEvent.result`；结果文本放在顶层 `content`。终端：`result.content.{exit_code, stdout, stderr}`，`result.status = "SUCCESS"` |
| `HEARTBEAT`、`CUSTOM`（`remove-tool`） | 可以忽略 |

进程最后还会打印两行纯文本：`requestID: ...` 和 `sessionID: ...`。还没开始一轮就出错时（比如模型不存在：`不支持该模型: x`），只打印纯文本，退出码为 1。

## 会话、取消、模型

- `--sessionId` 可以传任意 id。同一个 id 由另一个进程接着跑，上下文能延续（实测记住暗号后，在新进程里能答对）。
- 取消：`with-cli cancel --session-id <id> --request-id <message_id>`，两个参数都必须带。之后 chat 进程以退出码 0 结束，并且不发 `RUN_FINISHED`。
- `with-cli model list`：每行一个模型，开头是 id，后面跟 `supports-thinking`、`context=`、`thinking_effort=` 等。
- 每轮都可以用 `--model`、`--reasoning-effort`、`--enable-thinking`、`--access-mode` 来调。

## MCP

- `-c <yaml>` 会整体替换客户端配置（默认是 `~/.bg-agent/bg-agent-client.yaml`）。只写一行 `mcp_config_path` 的话，连模型都拉不到，所以要复制用户的配置，再在后面追加 `mcp_config_path`。
- MCP 文件的格式和 `~/.bg-agent/mcp_config.json` 一样：`{"mcpServers": {name: {...}}}`。stdio 服务写成 `"transportType": "stdio", "command", "args", "env"`，实测能正常调用，`env` 也能传到。

## 开场说明

`--user-rules <文件>` 可用：实测会话能遵守文件里的规则（冒烟测试中的 fixture word）。
