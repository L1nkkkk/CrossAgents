# cursor-cli 记录

2026-11-02，对接 `cursor-native` 驱动前的协议梳理。**本文来自设计确认，不是本机实测**：写这份记录时本机没装 Cursor CLI，命令、事件名与参数以官方文档和设计讨论为准，未逐条跑过；下文标「已确认」的是设计阶段定下的结论，装好 CLI 后仍需按 [with-cli.md](with-cli.md) 的方式补一次实测（尤其 `--stream-partial-output` 的去重规则和 MCP 挂载）。

## 结论

走 `agent -p --output-format stream-json --stream-partial-output`，每轮一个进程，会话靠 `--resume <chatId>` 在 Cursor 侧续上。输出是 NDJSON，一行一个事件，每行都带 `session_id`。驱动能拿到的比 Claude 侧少：没有逐次权限询问（只有 `--force`/`--yolo` 与 `--sandbox` 两档），没有 `--system-prompt`/`--user-rules`（开场说明得前置进第一条 prompt），没有 `--mcp-config`（MCP 只能从配置文件或临时 plugin 读），没有 cancel 子命令（打断只能杀进程）。stream-json 没有逐 token 事件，但终端 `result` 带 `usage`，驱动直接上报用量。因此能力表里 `permission_callback` 只能降级，`usage` 为 true。

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
- 用量：stream-json 没有逐 token 事件，但终端 `result.usage` 带逐轮 token 数，驱动直接上报，`usage` 能力为 true。

## 账号接入（2026-11-03 实测）

本机：Windows 11，Cursor CLI `2026.10.22-27dcf5f`，装在 `%LOCALAPPDATA%\cursor-agent\`（`agent.cmd` 调 `cursor-agent.ps1`，加 `-NoProfile -ExecutionPolicy Bypass -File`），该目录在用户 PATH 里；`builtin::cli("cursor").command()` 找到的就是 `...\AppData\Local\cursor-agent\agent.cmd`。下面带「实测」的是这台机器上跑过的；标「bundle 证据」的是从 CLI 打包代码里读出来的，没有可用的 Cursor 账号，跑不到那一步。

### `agent status --format json`

未登录（实测）：

```json
{ "status": "unauthenticated", "isAuthenticated": false, "hasAccessToken": false, "hasRefreshToken": false, "message": "Not logged in" }
```

已登录（bundle 证据 + 伪造凭据实测字段名）：`status: "authenticated"`、`isAuthenticated: true`、`hasAccessToken`、`hasRefreshToken`、`message`（拿不到资料时是 `Logged in (unable to fetch user details)`，否则缺省）、`userInfo: { email, userId, firstName, lastName, teamId, teamName, createdAt }`。只有 access token、缺 refresh token 时报 `partially-authenticated`，`isAuthenticated: false`。

一个坑（实测）：带 `userInfo` 的那条路径会去 `getMe` 拉资料，离线或 token 失效时仍报 `authenticated`，只是没有 `userInfo`、`message` 写「unable to fetch user details」。账号页据此显示「已登录、邮箱未知」并把 CLI 的话放进 note。

`CURSOR_API_KEY` / `CURSOR_AUTH_TOKEN` 只用于单次请求，不写进凭据库（实测：设 `CURSOR_API_KEY=dummy` 后 `status` 仍是 `unauthenticated`）。所以靠环境变量 API key 跑的 CLI 在账号页会显示未登录——这一版只认浏览器登录。

### `agent login`

`NO_OPEN_BROWSER=1 agent login` 实测（stdout 重定向到文件，非 TTY）：

```
Starting login process...
Authenticating with Cursor...
Waiting for browser authentication...
Open a browser and navigate to this link: https://cursor.com/loginDeepControl?challenge=...&uuid=...&mode=login&redirectTarget=cli&supportsSelectedTeamLogin=true
```

链接打在 **stdout**，不是只有 TTY 才有（上面就是管道里的输出；用 tokio 的 `Stdio::piped()` 读同样拿得到，provider 里临时验证过）。之后进程不退出，轮询 `https://api2.cursor.sh/auth/poll`（bundle 证据：`loginDeepControl` + `/auth/poll`），**没有本地回调端口**，所以不需要 `serves`。hub 的普通登录流程（抓链接 → 交 GUI 打开 → 等进程结束 → 读一次 status → 发 `account.login_finished`）正好对得上：链接约 2 秒内出现，进程在浏览器里点完之后自己退出。

没做到的实测：手上没有 Cursor 账号，浏览器那一步没人点，所以「登录完成后进程退出码为 0」这一条没跑到；hub 那边按 `退出码成功 && status.logged_in` 判断，没完成会报失败并带上 CLI 最后几行输出。

### `agent models`

需要**有效**登录（实测：假 token 也是 `Error: Authentication required...`，退出码 1，错误在 stderr）。输出是文本，不是 JSON（`agent models --help` 只有 `-h`）：

```
Available models

auto - Auto (default)
gpt-5
sonnet-4-thinking - Sonnet 4 Thinking (current, default)

Tip: use --model <id> ...
```

格式来自 bundle 里的 `models.ts`；本机没有登录，完整列表没实际看到过，所以 provider 只取「`Available models` 之后、空行之前」每行的首个词当 id（去掉颜色码和 `(current, default)` 标记），拿不到就退回 `crates/plugins/agents/cursor.toml` 的 `fallback = ["auto", "sonnet-4-thinking", "gpt-5"]`（实测：未登录时 `agent models` 失败，账号页给出的正是这三个）。

### 凭据放在哪

- Windows：`%APPDATA%\Cursor\auth.json`（实测：把包含两个凭据字段的假登录文件写进临时 `APPDATA\Cursor\auth.json` 后，`agent status` 报 `authenticated`；只写 accessToken 报 `partially-authenticated`）。
- macOS：`~/.cursor/auth.json`；Linux：`$XDG_CONFIG_HOME/cursor/auth.json`（bundle 证据：`getAuthFilePath` 按平台拼，域名是 `cursor`，Windows 上做 Title Case → `Cursor`）。
- macOS 的登录文件是**写死的 `~/.cursor/auth.json`**，不跟 `CURSOR_DATA_DIR` 走（bundle 证据：`case "darwin": join(homedir(), ".cursor", "auth.json")`），所以 macOS 上隔离登录只能改 `HOME`。
- `~/.cursor/cli-config.json` 只是设置（权限、沙箱、编辑器偏好），**不含 token**（实测看过本机文件）。
- `AGENT_CLI_CREDENTIAL_STORE=file|memory|default` 选存储：`file` 强制上面那个 `auth.json`，`default` 在 macOS 走钥匙串。

### 多账号隔离

- `CURSOR_DATA_DIR` 换数据目录（默认 `~/.cursor`：设置、`chats/`、`projects/`）。改它**不动登录**。
- 登录文件跟着平台的配置目录走，但**只需要改那一个变量**：
  - Windows：`APPDATA` → `%APPDATA%\Cursor\auth.json` 跟着走；
  - Linux：`XDG_CONFIG_HOME` → `$XDG_CONFIG_HOME/cursor/auth.json`；
  - macOS：`HOME`（因为上面的路径写死，`CURSOR_DATA_DIR` 搬不动它）。
- 再加 `AGENT_CLI_CREDENTIAL_STORE=file`（macOS 上把钥匙串也换成文件）。
- 其余一律不动：Windows 的 `USERPROFILE`/`HOME`/`LOCALAPPDATA`、Linux 的 `HOME` 都保持用户自己的，agent 在任务里跑的 git、ssh、npm 仍看到真实的 `~/.gitconfig`、`~/.ssh`、缓存。**只有 macOS 例外**：`HOME` 被挪到账号 profile 下，随它跑的 git/ssh/npm 也会看到 profile 里的文件，这是 macOS 上第二个登录的代价。
- provider 的 `agent_env` 因此给独立账号只有：`CURSOR_DATA_DIR=<home>`、上面那一个平台变量（指向 `<home>/profile...`）、`AGENT_CLI_CREDENTIAL_STORE=file`，外加把 `CURSOR_API_KEY`/`CURSOR_AUTH_TOKEN` 置空（CLI 按真值判断，空串等于没设）；登录命令里这两项改成 `env_remove`。本机登录（`default_home` = `~/.cursor`）不加任何变量，用机器上那一个登录。
- 实测（两个临时目录，只改 `APPDATA` + `CURSOR_DATA_DIR`，`USERPROFILE`/`HOME`/`LOCALAPPDATA` 保持用户自己的）：一个放了假凭据报 `authenticated`、另一个报 `unauthenticated`，互不影响。provider 层再验一次（两个独立 home 各跑一次 `status`）：放了假凭据的那个 `logged_in=true`、另一个 `false`，`agent_env` 只有 5 个变量，没有 `USERPROFILE`/`HOME`/`LOCALAPPDATA`。
- 没能实测：真正两个 Cursor 账号同时登录时的行为（没有账号）。从 bundle 看凭据路径只由平台目录决定，没有别的全局单例；风险在于 CLI 自己的后台进程（若有）可能仍读用户目录。

### 额度与 token（2026-11-07 实测）

两条通道分别提供逐轮 token 和账单周期额度：

- **逐轮 token**：两版已装 CLI 的 bundle 在构造 stream-json 终端 `result` 时分别以 `...ce?{usage:ce}:{}` / `...ae?{usage:ae}:{}` 带入来自 `turnEnded` 的 usage；实测字段为 `inputTokens`、`outputTokens`、`cacheReadTokens`、`cacheWriteTokens`、`reasoningTokens`。`wire.rs` 将 `result.usage` 捕获到 `Turn.usage`；驱动兼容实测的 camelCase 和 snake_case，字段缺失、改名、非数字或负数按 0 处理。每轮有 usage 时，在完成事件前发一个 `SessionEventKind::Usage`；整个 usage 缺失则不发。`cost_usd` 为空、`size` 为 0，`used` 是本轮 input + output，其中 input 包含 cache read/write；`tokens.{input,cached_input,output,reasoning_output,total}` 是会话累计值，cached_input 是 cache read，reasoning_output 是 output 的子集，total = input + output。`capabilities().usage=true`。hub 将它转成 `agent.usage`，供 `/v1/usage` 与 `usage_get().by_agent` 使用。
- **账单周期额度**：账户 provider 向 `https://api2.cursor.sh` 的私有 Connect-RPC `aiserver.v1.DashboardService/GetCurrentPeriodUsage` 发 Connect-JSON 请求，头为 `Content-Type: application/json`、`Connect-Protocol-Version: 1`，Bearer 凭据来自账户的 `auth.json` 的 `accessToken`。实测 `billingCycleStart`/`billingCycleEnd` 是 epoch 毫秒 int64 字符串，代码也接受数字；`planUsage` 的 `totalPercentUsed`、`autoPercentUsed`、`apiPercentUsed` 实测为数字。响应另有 `totalSpend`、`includedSpend`、`remaining`、`limit` 等字段，代码忽略它们。总百分比生成 `rate_limits` 的 primary 窗口和 `limits` 的首个 all 主组，另两个分别生成 auto、api；GUI 用 slice(1) 显示后两组。任一有效百分比达到 100 时 `limit_reached` 为 true，窗口展示百分比限制在 0–100，缺失或无效字段不编造。有效周期用于窗口时长和重置时间。套餐名只由 `agent about --format json` 的 `subscriptionTier` 获取，失败则留空。状态缓存 60 秒，请求超时 20 秒；读取失败降级为空额度并保留登录状态和提示。

私有 RPC 未文档化，可能无预告变更且没有版本保证；它涉及凭据，绝不把凭据写入日志或 `state.db`，也绝不硬编码百分比。逐轮通道只覆盖 CrossAgents 发起的 CLI 会话（每轮一个 `-p` 进程），不覆盖 IDE/网页端；它只上报 token，没有美元费用。`AccountProvider::sessions()` 仍返回空，因此按厂商日志统计的 `/v1/usage/series` 仍没有 Cursor 数字。

原计划的 `afterAgentResponse` hook 通道已排除：两版 bundle 加五次真实 CLI 实跑表明，两个已装版本的 headless `-p` 循环不走该 hook；触发点只在交互 TUI 提交处理器的 `3026.index.js` / `4525.index.js` 的 after-agent-hooks。门槛 `function o(e,t){return Boolean(e.userHooks?.hooks[t]?.length||e.projectHooks?.hooks[t]?.length)}` 只看 userHooks/projectHooks；`--plugin-dir` 经 `getPluginHooks` 放入独立的 pluginHooks，`--add-dir` 经 `619.index.js` 的 `q()` 放入 repoHooks，均不被门槛检查。runtimeHooks 仅是云 worker 选项；hook 配置路径只有 `~/.cursor`、`<workspace>/.cursor`、ProgramData 和 claude 系列。`sessionStart`/`sessionEnd` 会触发，但与逐轮 token 无关。

`store.db` 路线也已实测排除：730 个 blob 中只有 219 个 JSON 消息，字段仅有 role/content/providerOptions（modelName/modelProviderMessageId/requestId），其余 511 个为二进制，没有 token/usage/cost；`meta.json` 只有 createdAtMs/updatedAtMs/cwd，`~/.cursor/ai-tracking/ai-code-tracking.db` 只有 requestId/conversationId/timestamp/model。解析这些最多补会话数，`TokenTotals` 仍为 0。Enterprise Admin API（如 `/teams/filtered-usage-events`）需要 Enterprise 和团队管理员 API key，不在范围内。剩余缺口是 IDE/网页端用量与美元费用。


## 目录信任与沙箱（2026-11-05 本机实测）

Windows 11，Cursor CLI `2026.10.22-27dcf5f`，已登录。在全新临时目录执行 `agent -p --output-format=stream-json --stream-partial-output --trust --sandbox=enabled --workspace=<新目录> -- <prompt>`，退出码为 1，CLI 返回：

```text
Error: Sandbox mode is enabled but not available on this system. Sandbox requires macOS or Linux.
Run 'agent sandbox disable' to switch to allowlist mode.
```

没有出现目录信任提示，但进程在沙箱初始化时失败，不能据此确认会话能成功启动。由于没有进入模型会话，无法实测沙箱内运行 cargo 或调用 hub MCP 工具；macOS/Linux 上的这两项以及完整启动仍需实测。驱动保留 `--sandbox=enabled`，不采用错误提示中的关闭沙箱建议。

### Windows 非沙箱模式与 bypass（2026-11-05 本机实测）

同一 Windows 11 / CLI 版本，已登录，分别使用全新临时目录；以下结果只代表本次测试。

- `agent -p --output-format=stream-json --trust --workspace=<新目录> -- <prompt>`，不传 `--sandbox` 或 `--force`：会话启动并以退出码 0 完成。文件读取工具成功读到目录内的 `probe.txt`；shell 工具运行 `echo CURSOR_COMMAND_PROBE_7284` 返回 `rejected`，没有输出（模型自行重试一次，仍被拒绝）。这不能推断其他命令或配置的行为。
- 另一全新目录运行 `agent -p --output-format=stream-json --force --workspace=<新目录> -- <prompt>`，不传 `--trust`：未出现信任提示，会话返回 `FORCE_PROBE_OK`，result 为 success，退出码 0。此探针只验证启动和文本回复。

驱动不因为这次非沙箱实测而改变默认权限边界：Windows 非 bypass 会话在启动前报错，须明确启用 bypass。
