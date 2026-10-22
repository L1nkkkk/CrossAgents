# CrossAgents

CrossAgents 是一个分层的多 agent 编码编排器：**强模型（Opus / GPT-5 级）负责设计、拆解和验收，
便宜模型（Haiku / mini 级）并行写代码**。每个任务在独立的 git worktree 里完成，由 hub 亲自跑验收命令，
失败自动打回重试、必要时升级到更强的模型，通过后合并到一个集成分支，最终是否合入由你决定。

各家 agent 以插件形式接入（插件协议就是 [ACP](https://agentclientprotocol.com)）：内置 Claude Code 和 Codex，
任何讲 ACP 的 agent 都能通过配置或插件目录接入，不用改代码。

- 设计：[docs/DESIGN.md](docs/DESIGN.md)
- 各里程碑任务书：[docs/tasks/](docs/tasks/)

## 准备

| 需要 | 用途 |
|---|---|
| Rust 1.99（见 `rust-toolchain.toml`） | 构建 |
| git | 工作区与合并 |
| Node.js（`npx`） | 运行 Claude / Codex 的 ACP 适配器 |
| 已登录的 Claude Code 和/或 Codex | 真正干活的 agent；在终端里各自登录一次 |

```sh
cargo build --release
# 把 target/release/crossagents 放进 PATH
```

两个插件都默认走原生驱动：
- Claude 走 `claude -p` 的 stream-json 控制协议：先找 PATH 上的 `claude`，再找 `~/.local/bin/claude`，最后用 Claude 桌面版自带的那份。桌面版的登录 `claude` 用不上，要在终端里 `claude auth login` 登录一次。
- Codex 走 `codex app-server`：先找 PATH 上的 `codex`，找不到就用 Codex 桌面版自带的那份。

**代理**：Claude Code 不读 Windows 的系统代理。hub 环境里没有 `HTTPS_PROXY` 时，会把系统代理设置传给它（本机地址除外）。

**Codex 提示**：建议给 agent 显式指定模型。不指定时用 `~/.codex/config.toml` 里的默认模型，
它不一定支持你的登录方式（例如 ChatGPT 账号）。GUI 的模型下拉框列的是 Codex 为当前登录给出的模型
（`~/.codex/models_cache.json`），新建队友默认选第一个；已有队友可以在「编辑」里改，从下一轮起生效。

## 快速开始

最省事的方式是用引导接入（目前支持 Codex）：

```sh
crossagents setup codex      # 检查环境 → 选/导入账号 → 登录 → 显示套餐与额度 → 选模型 → 建好 planner 和 worker
```

也可以手动来：


```sh
crossagents hub start

# 在你的项目（git 仓库，至少一次提交）里登记 agent
crossagents agent add arch  --plugin claude --model opus  --role planner --cwd ~/proj
crossagents agent add coder --plugin claude --model haiku --role worker  --cwd ~/proj

# 发起一次编排
crossagents run "给项目加用户登录" --planner arch --worker coder
crossagents run show <id>          # Planner 提交计划后在这里查看
crossagents run approve <id>       # 批准后自动派发、验收、合并
crossagents run show <id> -v       # 跟踪任务；-v 显示验收输出与 diff
git merge crossagents/<id>         # 结果在集成分支上
```

常用选项：

| 选项 | 作用 |
|---|---|
| `--escalate <agent>` | 某任务连续失败 `--max-attempts` 次后，换这个更强的 agent 在同一 worktree 重做 |
| `--reviewer <agent>` | 每个通过验收的改动先交给它审查，驳回意见转给 Worker |
| `--no-approve` | 不等你审批计划，直接开工 |
| `--max-parallel N` | 同时进行的任务数（默认 3；修改范围重叠的任务自动串行） |

其他命令：

```sh
crossagents send coder "解释一下 src/main.rs" --wait   # 直接和某个 agent 对话
crossagents attach coder                              # 实时查看输出并交互
crossagents events --follow                           # 全部事件流
crossagents usage --by role                           # 强/弱模型各花了多少
crossagents run reject <id> "任务拆得再小一点"          # 驳回计划
crossagents run cancel <id>
```

所有命令都支持 `--json`。

## GUI

`apps/gui/` 是桌面 GUI（Electron + React），和 CLI 一样只是 hub 的客户端：先 `crossagents hub start`，
GUI 读取 `~/.crossagents/hub.json` 与 `token` 连接（同样遵循 `CROSSAGENTS_HOME`）。
界面以对话为中心：侧边栏按 run 列出各个会话，点进去就是和它的对话；Planner 的计划卡片直接出现在对话里，
批准后任务实时推进。每轮回复下有一行改动摘要（改了哪些文件、增删多少行），右侧工具列可以按轮看 diff、看任务详情和原始事件流。
主题在「设置」里选：跟随系统、浅色（素白 / 纸 / 雾）、深色。

```sh
cd apps/gui
npm install
npm run electron:dev       # 开发（页面热重载）
npm run preview:mock       # 不起 hub，在浏览器里用假数据看界面；右下角切场景
npm run electron:build     # 产出 release/win-unpacked/CrossAgents.exe（免安装，未签名）
```

令牌只在 Electron 主进程里使用，不会进入页面。详见 [V2 任务书](docs/tasks/V2-GUI.md)（外壳与安全）和 [V3 任务书](docs/tasks/V3-GUI.md)（对话式界面）；
[V6 任务书](docs/tasks/V6-GUI.md)：mock 预览、视觉层次、按轮 diff 与工具列。

## 账号管理

每个 agent 可以绑定一个账号（`agent add --account <id>`），同一家可以有多个账号。

```sh
crossagents account ls
crossagents account add codex-main --import-default   # 用现有的 ~/.codex 登录
crossagents account add codex-work                    # 新账号，独立目录
crossagents account login codex-work                  # 打开浏览器登录
crossagents account status codex-main                 # 套餐、额度窗口与重置时间、余额（实时）
crossagents account usage codex-main --by model       # token、缓存命中；还可按 day/origin/agent/role/run
crossagents account models codex-main                 # 这个账号能用的模型
crossagents account add claude-work --provider claude # Claude 账号同样管理（独立的 CLAUDE_CONFIG_DIR）
```

令牌只在 hub 进程内用于查询额度，任何 API 和日志都不会输出令牌，邮箱打码显示。
实时查询失败（没网、登录过期）时，回退到 Codex 会话日志里最近一次记录的额度，并标明时间。

## 配置

数据在 `~/.crossagents/`（可用环境变量 `CROSSAGENTS_HOME` 改）。`config.toml`：

```toml
[hub]
port = 7420

# 覆盖内置插件的启动命令，或注册任意 ACP agent
[plugins.claude]
native_command = ["C:/tools/claude.exe"]   # 不写就自动找


[plugins.gemini]
command = ["gemini", "--experimental-acp"]

# 要改回 ACP 适配器（codex-acp、claude-agent-acp，需要 Node.js）：
[plugins.codex]
driver = "acp-generic"
```

单个 agent 也可以用 `crossagents agent add … --driver codex-native|claude-native|acp-generic` 覆盖插件的默认驱动。
Claude agent 的 `config` 里还可以写 `permission_mode`（如 `acceptEdits`）、`effort`、`setting_sources`
（默认 `project,local`，不带你自己的 Claude Code 插件、hook 和连接器）。
驱动与能力见 [N1 任务书](docs/tasks/N1.md)。

也可以把插件放成目录：`~/.crossagents/plugins/<name>/plugin.toml`

```toml
name = "my-agent"
protocol = "acp"
command = ["./bin/my-agent", "--acp"]   # 相对路径相对插件目录
models = ["fast", "smart"]
```

## 工作方式

```
你 ──run──▶ Planner（强模型）──plan_submit──▶ 你审批
                                              │
        ┌─────────────── hub ─────────────────┘
        ▼
  每个任务：新 worktree + 新 Worker 会话（弱模型）
        │ task_complete
        ▼
  hub 提交并跑验收命令 ──失败──▶ 打回重试 ──多次失败──▶ 升级 / 失败
        │ 通过
        ▼
  （可选）Reviewer 审查 ──▶ 合并到 crossagents/<run> ──▶ 通知 Planner
```

Agent 通过 hub 挂载的 MCP 工具协作（`plan_submit`、`task_complete`、`task_ask` 等），
每个会话有独立令牌，只能使用自己角色的工具。

## 本地 API

hub 只监听 `127.0.0.1`。除 `GET /v1/health` 外都要 `Authorization: Bearer <~/.crossagents/token>`。
`GET /v1/events` 是 SSE 事件流（支持断点续传，`?agent=` 只看某个会话）；`GET /v1/agents/{key}/transcript` 返回按轮分组的对话历史（每轮带改动 stat），`GET /v1/agents/{key}/turns/{message_id}/diff` 返回某一轮的完整 diff（CLI：`crossagents session diff <agent> [--turn N] [--stat]`）。GUI 用的 TypeScript 类型由
`cargo test -p crossagents-api-types` 生成到 `bindings/`。

## 开发

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

端到端测试用 `crates/mock-agent`：一个可编程的 ACP agent，按提示里的 `[write …]`、`[call …]` 指令行事，
不需要任何 LLM 或账号就能跑通完整流水线。
