# 豆包接入调研

2026-10-28。依据：火山方舟官方文档（Agent Plan / Coding Plan / 按量计费）、TRAE CLI 2.0 命令行文档、本仓库代码。

**均未真机验证**——本机 `PATH` 上没有 `traecli`、`claude`、`codex`。结论是"这条路存在且形态对得上"，不是"已经跑通"，验证清单见文末。

## 结论

"把豆包接进来"其实是三件事，成本差一个数量级：

| # | 接入的是 | 做法 | 改代码 | 现在能不能用 |
|---|---|---|---|---|
| A | 豆包当 **agent**（像 Claude Code 一样被编排） | `traecli acp` 走现成的 `acp-generic` | 不改，一段 `config.toml` | 能，但要先验 ACP 质量 |
| B | 豆包当 **模型**（Claude Code / Codex 的脑子） | 方舟兼容端点 + 账号目录里的配置 | 不改 | 能，两处坑要绕 |
| C | 豆包当 agent 的**原生驱动** | 新 `trae-native`，走 `traecli exec --json` | 要写，约 800–1500 行 | 等 A 暴露缺口后再做 |

先做 A，半小时内能出结论。B 是并行的另一条线：它不产生"豆包 agent"，但能让 Claude Code 的壳跑豆包的模型——对编排质量通常比换壳更划算。C 只有在 A 的能力缺口（权限、MCP、resume）补不上时才值得动。

## A. 豆包作为 agent：`traecli acp`

TRAE CLI 2.0 的子命令表里有 `acp`——"以 ACP（Agent Client Protocol）server 运行，供外部客户端接入"。这正是 `acp-generic` 的对接方式，而本仓库加一家 agent 本来就不用改代码（`crates/plugins/src/lib.rs:1-21`、DESIGN §9.3）：没有 `driver` 的插件默认就是 `acp-generic`（`crates/core/src/driver.rs:41-44`）。

`~/.crossagents/config.toml`：

```toml
[plugins.trae]
command = ["traecli", "acp"]          # 不写 driver，默认 acp-generic
models = ["<从 traecli 里抄来的模型 id>"]  # 不写也行，只是界面少一个下拉
```

要点：

- `traecli` 的认证在 CLI 自己身上（`traecli login` / `login --with-trae-pat` / `login status`），不需要本仓库的账号体系；`acp-generic` 不绑账号。
- 模型选择靠 ACP 的 session 配置，`PluginConfig.model_env` 那套（`crates/plugins/src/lib.rs:56-58`）对它未必有效——先不设 `model_env`，用 TRAE 自己的默认模型。
- 协作工具（`crossagents mcp`）挂在 ACP 的 `session/new` 里，`acp-generic` 已经会传；但 with-cli 的前车之鉴是 ACP 握手正常、工具调用全失败（`docs/research/with-cli.md`），所以第一条冒烟测试必须是"让它读一个文件"，不是"让它说句话"。

## B. 豆包作为模型：方舟兼容端点

方舟对外是两套协议、四五个端点，选错了钱会从另一个口子流走：

| 端点 | 协议 | 走哪笔钱 |
|---|---|---|
| `https://ark.cn-beijing.volces.com/api/plan` | Anthropic（`/v1/messages`） | Agent Plan 套餐 |
| `https://ark.cn-beijing.volces.com/api/plan/v3` | OpenAI Responses | Agent Plan 套餐（Codex 用这个） |
| `https://ark.cn-beijing.volces.com/api/coding` | Anthropic | Coding Plan 套餐 |
| `https://ark.cn-beijing.volces.com/api/coding/v3` | OpenAI | Coding Plan 套餐 |
| `https://ark.cn-beijing.volces.com/api/v3` | OpenAI Chat / Responses | 按量计费，**不走套餐额度** |

套餐 Key 与按量 Key 不互通。配完先用 `/status` 核一遍模型和连通性，再看一眼控制台是哪个口子在扣钱。

### B1. Claude Code（推荐这条）

账号的 `home` 就是 Claude 的配置目录（`CLAUDE_CONFIG_DIR`，`crates/plugins/src/claude_account.rs:60`），所以把方舟配置写进这个目录就天然是"一个账号 = 一套端点"：

```bash
crossagents account add doubao --provider claude --home ~/.crossagents/accounts/doubao
```

`~/.crossagents/accounts/doubao/settings.json`（照官方文档，模型名按套餐支持列表换）：

```json
{
  "env": {
    "ANTHROPIC_AUTH_TOKEN": "<Agent Plan API Key>",
    "ANTHROPIC_BASE_URL": "https://ark.cn-beijing.volces.com/api/plan",
    "ANTHROPIC_MODEL": "ark-code-latest",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "ark-code-latest",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "ark-code-latest",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "ark-code-latest",
    "CLAUDE_CODE_SUBAGENT_MODEL": "ark-code-latest",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1"
  }
}
```

同目录再放 `.claude.json`：`{"hasCompletedOnboarding": true}`，省掉首次引导。

**坑 1：默认不读这份 settings.json。** `claude-native` 起进程时带的是 `--setting-sources project,local`（`crates/drivers/src/claude_native/wire.rs:134`），`user` 这一档被排除了，而账号目录里的 `settings.json` 正属于 `user`。要给这个 agent 的 config 加上：

```json
{ "setting_sources": "user,project,local" }
```

（读取点：`crates/drivers/src/claude_native/mod.rs:116`。）

**坑 2：代理预检探错主机。** `check_proxy` 用 `ANTHROPIC_BASE_URL` 推出要探的 host（`crates/drivers/src/claude_native/wire.rs:114`、`mod.rs:587`），而这个值只从会话 env 或 hub 自身环境里找；账号注入的 env 只有 `CLAUDE_CONFIG_DIR`，所以它探的是 `api.anthropic.com`。后果分两种：

- 配了代理但代理到不了 Anthropic → 开会话直接失败，报的是代理；
- 代理能到 Anthropic → 预检通过，但方舟的流量也跟着走代理，慢或超时。

绕法：`NO_PROXY` 里加 `ark.cn-beijing.volces.com`；或把 `ANTHROPIC_BASE_URL` 放进 hub 自己的环境（代价是所有 Claude agent 都改走方舟）。

**坑 3：这个账号在界面上是"未登录"。** 它没有 Anthropic 的 OAuth，`status` 查出来 `logged_in = false`，额度面板也读不到方舟的套餐余量。不影响开会话（没有登录门禁），但会让人以为配错了。想消掉这个违和感，就得加一个 `ark` 账号 provider——那是 C 量级里最小的一块代码（`crates/plugins/src/claude_account.rs` 同构，`agent_env` 直接返回 `ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN`，约 20 行），顺带把坑 1、坑 2 一起解决。

### B2. Codex

Codex 的 provider 配置本来就在账号目录里（`CODEX_HOME`，`crates/plugins/src/codex_account.rs:570`），所以这条路比 Claude 干净，不需要 `setting_sources`：

```toml
# ~/.crossagents/accounts/<id>/config.toml
model = "ark-code-latest"
model_provider = "volcengine-agent-plan"
model_supports_reasoning_summaries = true

[model_providers.volcengine-agent-plan]
name = "volcengine-agent-plan"
base_url = "https://ark.cn-beijing.volces.com/api/plan/v3"
env_key = "ARK_API_KEY"
wire_api = "responses"
```

代价是 `env_key` 指向的变量必须真的在进程环境里，而账号只注入 `CODEX_HOME`——要么写进 hub 的环境，要么等上面那个 `ark` provider。

## C. 豆包作为原生驱动：`trae-native`

`traecli` 的形态和 `with-cli` 几乎同构，所以照 `with_native` 抄就行：

| 能力 | traecli | 对应 |
|---|---|---|
| 一轮一进程 + 事件流 | `traecli exec --json "<prompt>"`（JSONL 到 stdout） | 同 `with-cli chat -o stream-json` |
| 会话续接 | `--resume <SESSION_ID>` / `traecli resume <id> --last` | 同 `--sessionId` |
| 分叉 | `traecli fork <id>` | 同 `fork` |
| 权限 | `-a/--ask-for-approval`、`--permission-mode`、`-s/--sandbox` | 映射 `PermissionPolicy` |
| 工作目录 | `-C/--cd`、`--add-dir` | 同 `cwd` |
| MCP | `traecli mcp` 子命令 | 同 `-c <yaml>` |
| 模型 / effort | `-m/--model`、`-c key=value` | `set_model` |

社区还记录了一种 `exit code 2` + `pending-permission-*.json` 的跨轮协议（把审批状态写文件、用 `-c -p ""` 续接），官方命令行文档里没有，要用就得实测。

工作量参照：`with_native` 834 + 882 行，`claude_native` 970 + 1664 行。需要动的地方就三处——`DriverKind` 加 variant 与 `ALL` 数组（`crates/core/src/driver.rs:30`、`:48`）、构造分支（`crates/plugins/src/lib.rs:173`）、描述文件 `crates/plugins/agents/trae.toml`。

## 顺带两条

- 方舟套餐里不止豆包：`glm-5.3`、`deepseek-v4.1-flash`、`kimi-k3.1-pro` 都在同一个 Key 后面。所以"接方舟"实际是"接一排模型"，豆包只是其中一个——这反而更适合 CrossAgents 的编排（planner 用强的、worker 用便宜的）。
- 名字池里已经有 `doubao` 了（`crates/core/src/names.rs:48`），agent 取名不用另外加。

## 验证清单

1. 装 `traecli`（官方文档用这个名字，带 `traecli update` 自更新；社区文章里出现过 `npm i -g @trae/cli`，以 docs.trae.cn 的安装页为准），`traecli login status` 确认已登录。
2. A 路线：加 `[plugins.trae]` → 建 agent → 发一句"读一下 README 的第一段并复述"。读文件成功才算 ACP 通；改文件、跑命令、权限请求、中断、resume、MCP 调用依次过一遍。
3. B 路线：按 B1 建账号 + `setting_sources`，`crossagents account status doubao` 看到"未登录"是预期的；开会话发一句话，看是否回的是豆包；再 `tail` 一下 hub 日志确认 base URL 没被代理吃掉。
4. 两条都通之后，比较同一任务在 A、B 下的表现再决定把哪条写进 `docs/TODO.md`。
