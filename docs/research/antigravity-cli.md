# antigravity-cli 记录

Google Antigravity CLI：命令是 `agy`，Go 写的多 agent 终端工具。2026-06 起接替**个人版**
Gemini CLI（免费、Google AI Pro、Ultra）；企业 Gemini Code Assist 和按量 API key 仍走 `gemini`。
所以同一个 Google 账号，Pro 档的新模型在 `agy` 里，不在 `gemini` 里。

本记录对应 `agy 1.5.9`（2026-11 安装）。实测过：安装、`--help`、`models`、`mcp`，以及
未登录时跑一轮 stream-json（见「未登录时的一轮」）。**没有**跑通过成功的一轮（这台机器上
没登录 Google），事件形状与退出码另外取自官方
[headless 文档](https://antigravity.google/docs/cli/headless)，标为「文档」。

## 结论

- 一个进程可以吃多轮：`--input-format stream-json --output-format stream-json`，一条
  `{"event":"user","message":{"content":"…"}}` 一行写进 stdin，每轮以一个 `result` 结束。
  这比 `agy -p` 一轮一进程快，也省掉了每轮的启动。
- 会话 id 是 CLI 给的（`init` 与 `result` 的 `conversation_id`），`--conversation <id>`
  续一个旧会话。进程没了（打断、崩溃）就重开一个，带上这个 id。
- headless 没有逐次审批通道，但**不会因此失败**：需要批准而拿不到的工具被「soft-deny」，
  这一轮继续跑、退出码仍是 0，stderr 写一行说明。
- MCP 没有按会话注入的办法：`--settings` 明确拒绝 `mcpServers`（实测），配置只读
  `~/.gemini/config/mcp_config.json`，而 `agy mcp add` 写的就是这一份（对所有会话生效）。
  驱动的能力表因此报 `mcp: false`。
- 凭据在系统钥匙串（Windows 凭据管理器 / macOS Keychain / Linux secret-service），不在
  `~/.gemini` 里；换 `HOME` 不会换账号。`agy models` 未登录时打印
  `Please sign in to view available models.` 并以 1 退出（实测）。

## 命令行（`agy --help`，实测）

```
-p, --print, --prompt            非交互跑一条提示词，结果打到 stdout
    --output-format              text | json | stream-json（默认 text）
    --input-format               text | stream-json；stream-json 从 stdin 一行一条消息，
                                 每条跑一轮，且必须配 --output-format stream-json
    --conversation <id>          按 id 续会话
-c, --continue                   续最近一次会话
    --model <slug>               本次会话的模型（`agy models` 列）
    --effort <level>             low | medium | high | xhigh | max
    --mode <mode>                accept-edits | auto | plan（默认模式即不传）
    --dangerously-skip-permissions   所有工具调用一律批准
    --sandbox                    终端沙箱
    --add-dir <path>             额外工作目录（可重复）
    --settings <json|path>       只对本次会话生效的设置，不落盘（未知键会报错）
    --print-timeout <dur>        print 模式的时限，0 表示不限
```

子命令：`acp`（ACP stdio 服务）、`models`、`agents`、`mcp add|remove|list|enable|disable`、
`plugin`、`install`、`update`、`changelog`。

`--settings` 的键是 `settings.json` 的键：`{"nonsense":1}` 会报
`--settings: unknown setting "nonsense"`，`{"mcpServers":…}` 同样报未知（实测）。

## stream-json 的输出（文档）

一行一个 JSON 对象，顺序是 `init` → 若干 `step_update` → `result`：

```json
{"event":"init","conversation_id":"c3b6…","init":{"cwd":"/w","tools":["run_command"],"permission_mode":"request-review"}}
{"event":"step_update","step_update":{"conversation_id":"c3b6…","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"Git rebase "}}
{"event":"step_update","step_update":{"conversation_id":"c3b6…","step_index":4,"state":"DONE","step_type":"tool","tool_name":"run_command","tool_info":{"name":"run_command","parameters":{"CommandLine":"echo hi"},"output":"hi\r\n"}}}
{"event":"result","result":{"conversation_id":"c3b6…","status":"SUCCESS","response":"…","duration_seconds":6.9,"num_turns":1,"usage":{"input_tokens":10418,"output_tokens":589,"thinking_tokens":551,"cache_read_tokens":8113,"total_tokens":11007}}}
```

- `step_update.state`：`ACTIVE` 进行中，`DONE` 结束；`step_type`：`user_input`、
  `agent_response`、`tool`、`checkpoint`。回答的分片在 `agent_response` 的 `text_delta`，
  流式时先若干 `ACTIVE` 再一个 `DONE`。
- 工具步骤的 `tool_info` 有 `name`、`parameters`、`output`，失败时 `error:{type,message}`。
- `result.usage` 是**整个会话的累计**（包含通过 `--conversation <id>` 跨进程续上的历史轮次），
  不是单轮增量；`response` 才是这一轮的。
- **上下文占用与 Token 口径（2026-11-26 实测 `agy 1.5.14`）**：
  - `step_update.usage`（每个模型请求步）和 `result.usage` 都会上报 `input_tokens`、
    `output_tokens`、`thinking_tokens`、`cache_read_tokens`、`total_tokens`。
  - **`input_tokens` 与 `total_tokens` 不包含 `cache_read_tokens`**（实测 Turn 1：`input=9789, cache_read=0, out=1, total=9790`；Turn 2：`input=2116, cache_read=7744, out=1, total=2117`，真实上下文为 `2116 + 7744 + 1 = 9861`）。`thinking_tokens` 是 `output_tokens` 的子集。
  - `antigravity-native` 因此将单步真实输入计为 `input_tokens + cache_read_tokens`、上下文占用计为最后一步的 `input + cache_read + output`，并在每个 `step_update.usage` 立即上报 `SessionEventKind::Usage`（不再等到整轮结束）；对 `--conversation` 续会话首次 `result.usage` 自动扣除历史基线，避免重连后翻倍计入。
  - 省略 `--model` 时 `init` 事件不带 `model` 字段，驱动回退读取 `~/.gemini/antigravity-cli/brain/<conversation_id>/.system_generated/logs/transcript.jsonl` 中的 `model` 以推导上下文窗口大小 `size`（GUI 要求 `size > 0` 才显示占用条）。
  - **上下文分类拆解不可用**：实测 `agy -p /context --output-format json` 返回错误 `"/context is not available in print mode (it opens the interactive context breakdown)"`，headless 模式只能拿到总占用与累计 token，无法像 Claude Code / Qoder 那样按系统提示词 / 工具 / 消息分项拆解。
- `status`：`SUCCESS`、`ERROR`、`CANCELED`、`INTERRUPTED`、`INVALID`、`WAITING`、`RUNNING`。
- 退出码：成功 0；失败非 0（`ERROR` 时 `error` 里有原因）。stream-json **输入**模式下，
  控制消息或 CLI 自己处理的斜杠命令（`/model`）会让会话以 2 结束，坏 JSON / 缺 `event`
  / 非 `text` 内容块以 1 结束。

## 权限（文档）

- 默认（不传 `--mode`）：需要批准的动作在 headless 下**soft-deny**，这一轮继续。
- `--mode accept-edits`：改文件自动批准；`auto` 还要沙箱，部分账号不可用；`plan` 只读。
- `--dangerously-skip-permissions`：全批准，`init.permission_mode` 变成 `always-proceed`。
- 细粒度规则写在 `~/.gemini/antigravity-cli/settings.json` 的 `permissions.allow`
  （`command(git)`、`write_file(src/)` 这种）。

驱动把 `PermissionPolicy` 映射成：`Bypass` → `--dangerously-skip-permissions`，
`Guard` → `--mode accept-edits`，`Deny` → 不传（默认模式，会问的一律拒）。

## MCP（实测）

- `agy mcp add <name> <command> [args…]` 写到 `<home>/.gemini/config/mcp_config.json`，
  形如 `{"mcpServers":{"<name>":{"command":"…","disabled":false}}}`。
- `agy mcp list` 只列这一份全局配置；工作区 `.agents/mcp_config.json` 不会被它列出来。
- 没有 `--mcp-config` 一类的按会话入口；`--settings` 不接受 `mcpServers`；`--gemini-dir`
  只有 `acp` 子命令有。所以驱动挂不了协作工具，能力表 `mcp: false`，会话里发一条说明。

## 未登录时的一轮（实测）

把一条 `user` 消息写进 stdin 后，stdout 什么都没有，**stderr** 打印：

```
Authentication required. Please visit the URL to log in:
  https://accounts.google.com/o/oauth2/auth?…redirect_uri=https%3A%2F%2Fantigravity.google%2Foauth-callback…
Waiting for authentication (timeout 60s)...
Or, paste the authorization code here and press Enter:
```

然后它**在 stdin 上等授权码 60 秒**；没人贴就以 1 退出，并补一条
`{"event":"result","result":{"status":"ERROR","error":"authentication failed or timed out",…}}`。

两个结论：驱动在没有登录时会卡住一轮（约 60 秒）再报错，这是 CLI 的行为，不是驱动加的；
另外 headless 也能收授权码，登录另有专门入口（见下一节）。

## 登录（实测）

`agy acp --login` 是 CLI 自带的非交互登录（`agy help acp` 里的「Log in with Google
interactively, then exit (ACP terminal auth)」）。实测：

1. stdout 打印授权链接（**stdout**，不是 stderr）：

   ```
   Open this link in a browser to log in with Google:

     https://accounts.google.com/o/oauth2/auth?…redirect_uri=https%3A%2F%2Fantigravity.google%2Foauth-callback…

   Then paste the authorization code, or the full URL of the page you land on, and press Enter:
   ```

2. 然后在 stdin 上等一行：授权码，或者登录后落地页的完整地址都收。
3. **没有自己的超时**：stdin 开着就一直等（实测 200 秒仍在等）；stdin 关了才以
   `login cancelled: no authorization code was entered` 退出，退出码 0。
4. 码不对时打印 `couldn't log in: token exchange failed: oauth2: "invalid_grant" …`，退出码 1，
   进程结束；要重试得重开一个进程（链接也会换新的）。
5. 登录成功后退出码 0，凭据进钥匙串。

hub 的登录命令拿到的是**关闭的 stdin**，所以码没处可写。补上的一半是
`crossagents agy-login --port <n> --command <agy>`（`crates/drivers/src/antigravity_native/login.rs`）：
起 `agy acp --login`，读它的链接，在 `127.0.0.1:<n>` 上起一个单字段页面（链接 + 粘贴框），
把粘进来的内容写回 CLI 的 stdin；账号页把这个页面地址当登录页给 GUI（`LoginCommand::serves`，
和 CodeBuddy 的 Web UI 同一条路）。码被拒时页面显示 CLI 的原话并换上新链接。

`agy acp`（JSON-RPC）的 `initialize` 也列了 `oauth-personal` 等四个 `authMethods`，但
`authenticate` 既不开浏览器也不打印链接，90 秒内无任何回应——所以没走 ACP，走了 `--login`。


## 会话与恢复

- `--conversation <id>` 按 id 续；`-c` 续最近一次。
- 会话文件按项目存（`~/.gemini/projects.json`、`~/.gemini/tmp/<project>/`），但换目录能否
  续没有实测，驱动的 `move_cwd` 保守报 false。
- 进程常驻时不需要 `--conversation`：会话就在这个进程里。

## 账号额度与套餐（2026-11-16 补充）

- 官方 [headless 文档](https://antigravity.google/docs/cli/headless/) 支持把 CLI 自己处理的
  `/usage` 作为单独的 `--print` 查询；不能塞进常驻 stream-json 会话。
  账号提供器分别运行 `agy -p /usage --output-format json` 和
  `agy -p /credits --output-format json`，一分钟内缓存结果，不读取钥匙串凭据。
- 安装的 1.5.9 二进制中 printmode JSON 类型可确认的字段：quota 的 `groups`，group 的
  `name` / `buckets`，bucket 的 `name` / `window` / `disabled` /
  `remaining_fraction` / `remaining_amount` / `reset_time`；credits 的
  `remaining_credits` / `plan_name`。剩余比例转为已用比例；缺失、非法或仅有剩余数量的
  桶不伪造百分比。套餐来自 credits 的 `plan_name`，邮箱仍无可靠查询入口。
- 成功查询的真实额度**尚未验证**：这台机器的 `models` 能列出模型，但 `/usage` 被上游以
  `Eligibility check failed: ... not currently available in your location` 拒绝。提供器保留
  已登录状态，把错误放在账号备注，不将其显示为零额度；成功报告结构用固定样例回归测试。
- [额度文档](https://antigravity.google/docs/cli/commands/usage) 说明 Gemini 与 Claude/GPT
  有各自共享的模型组；模型组的显示名用于识别适用模型家族，未知组不当作所有模型的全局限额。
  [AI credits](https://antigravity.google/docs/cli/credits) 是基线额度用尽后的可选超额消费，
  余额为零不表示订阅基线已耗尽。
- UI 与账号 API 只允许登记本机共用登录，避免把不同目录误当成独立的 Antigravity 账号。

## 模型（实测 + 文档）

- `agy models` 需要登录：未登录打印 `Please sign in to view available models. Launch the CLI
  without arguments to sign in.`，退出码 0。
- 文档里的 slug 形如 `gemini-3.8-flash-medium`（effort 可以并进 slug），也支持
  `--model <slug> --effort <level>` 分开给。`--model` 写错时 headless **不会**回退，
  直接以 1 退出并把可用模型列在 `error` 里。
- 驱动解析 `agy models` 的文本：取像 slug 的 token（小写、含数字、含 `-`/`.`）或 `auto`，
  去重保序；解析不出来就用描述文件里的 fallback。

## 登录与凭据（文档）

- 首次跑 `agy` 走交互式引导：选主题、接受条款、信任目录，然后是 Google OAuth。
- 凭据存在系统钥匙串（Apple Keychain / Linux secret-service / Windows Credential Manager），
  不是 `~/.gemini` 下的文件。所以：换 `HOME`/`USERPROFILE` 不会换账号，也不会丢登录；
  headless 直接用缓存凭据，没有缓存时报 `authentication required` 而不是挂起。
- 配置：`~/.gemini/antigravity-cli/settings.json`（键 `model`、`agentMode`、`toolPermission`
  等）、`~/.gemini/antigravity-cli/updater/`（自更新锁，可用 `AGY_CLI_DISABLE_AUTO_UPDATE=true`
  关掉）、`~/.gemini/config/mcp_config.json`、工作区 `.agents/`（skills、rules、plugins）。

## 安装与定位（实测）

- Windows：`irm https://antigravity.google/cli/install.ps1 | iex`，落到
  `%LOCALAPPDATA%\agy\bin\agy.exe`。脚本从
  `https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/windows_amd64.json`
  取版本、URL 和 sha512，校验后复制；随后跑 `agy install` 改 PATH。
- macOS / Linux：`curl -fsSL https://antigravity.google/cli/install.sh | bash`，落到
  `~/.local/bin/agy`。
- 自更新：`agy update`，或后台自动（`~/.gemini/antigravity-cli/updater/update.lock`）。
- 国内网络注意：`storage.googleapis.com` 下载二进制这一跳经常握手失败，`antigravity.google`
  和 Cloud Run 的 manifest 却能通；代理要能到 Google Storage 才行。
