# CrossAgents 设计文档（v1）

> 状态：草案 · 2026-10-21

## 1. 定位

**CrossAgents 是一个分层的多 agent 编码编排器。** 强模型（Opus / GPT-5 级）负责理解需求、设计、拆解任务与验收；
便宜模型（Haiku / mini 级）并行完成具体编码任务。每个角色可以选择不同厂商、不同档位的模型。

CrossAgents 本身是一个 **core**；各家 agent（Claude Code、Codex……）以 **插件** 的形式接入，每个插件可以通过不同的
**接入方式**（driver）连接到具体的 agent。

### 1.1 目标

- 强模型规划、弱模型编码的分层协作，自动验收、失败重试与升级。
- 不同厂商的 agent 通过统一协议互相发消息、委派任务、回传结果。
- 新 agent 只需写一个插件即可接入；新需求通过扩展点加入，不改核心。
- 本地单机运行。v1 用 CLI 操作；v2 提供 GUI（Tauri）。本地 API 从 v1 起就按 GUI 客户端的标准设计。

### 1.2 非目标（v1 不做）

- 跨主机、多用户、设备配对
- GUI（v2 做，见 §11.1）；不做 TUI，避免与 GUI 重复投入
- 动态加载的二进制插件（dll/so）
- 自由编排（Planner 自主调度）——留作 `Pipeline` 的后续实现

## 2. 架构原则

1. **依赖只朝内**：`core` 只含领域模型、接口和事件，不依赖任何具体实现；实现依赖 `core`，反之不行。
2. **组件间用事件通信**：任务状态、agent 输出、用量都是事件。GUI、成本统计、日志、通知都是订阅者。
3. **扩展点用 trait**：只在最可能变化的维度上抽象（见 §5），其余保持简单，不为 v1 用不到的东西预留抽象。
4. **配置驱动**：角色、模型、流程、验收命令、重试策略都在配置里，换组合不改代码。
5. **协议带版本**：插件协议、MCP 工具、本地 API 的 schema 都带版本号。

## 3. 总体结构

```
┌──────────────────── CrossAgents Core ────────────────────┐
│  Orchestrator(Pipeline/Stage/Policy) │ 协作层(MCP 工具)    │
│  事件总线 │ Store │ WorkspaceProvider │ Verifier │ 本地 API  │
│              ▲ 只认识统一的 Agent 接口 ▲                    │
└──────────────┼────────────────────────────────────────────┘
               │  CrossAgents Plugin Protocol（统一命令 + 事件）
┌──────────────┼────────────────────────────────────────────┐
│ 插件层（"接谁"）：claude │ codex │ gemini │ ...               │
│ 身份、模型列表、能力声明、配置、认证检查、MCP 挂载方式         │
└──────────────┼────────────────────────────────────────────┘
┌──────────────▼────────────────────────────────────────────┐
│ 接入方式 Driver（"怎么接"）：                                │
│ ACP（v1 首选）│ stdio-jsonl │ json-rpc │ HTTP API │ PTY      │
└───────────────────────────────────────────────────────────┘
      ▲ CLI / GUI 只通过本地 API（HTTP + SSE）与 hub 交互
```

## 4. 核心概念

| 概念 | 含义 |
|---|---|
| **Agent** | 命名的 agent 实例：插件 + driver + 模型 + 角色 + 配置 |
| **Role** | `planner` / `worker` / `reviewer`，决定会话策略和可用工具 |
| **Session** | agent 背后的会话；状态 `idle / running / stopped` |
| **Run** | 一次用户需求的完整编排过程（一棵任务树） |
| **Task** | 一次委派，带结构化规格（§7），有状态机 |
| **Message** | agent 之间的单向通知 |
| **Workspace** | 任务执行的隔离环境（v1：git worktree） |
| **Event** | 系统内一切变化的记录，持久化并广播 |

### 4.1 角色

| 角色 | 典型模型 | 职责 | 会话策略 |
|---|---|---|---|
| Planner | Opus / GPT-5 级 | 设计、拆解、写规格、整合结果、处理冲突 | 长期会话，保留项目上下文 |
| Worker | Haiku / mini 级 | 按规格编码 | 每个任务一个新会话 |
| Reviewer（可选） | 中档或强模型 | 审查 diff，通过或打回 | 每个任务一个新会话 |

## 5. 扩展点

| trait | 抽象什么 | v1 实现 | 以后可加 |
|---|---|---|---|
| `AgentPlugin` | 一家 agent 的接入 | claude、codex（内置）、`ExternalPlugin` | gemini、aider、直连 API |
| `Driver` | 连接 agent 的方式 | ACP | 原生 stdio-jsonl / json-rpc、HTTP、PTY |
| `WorkspaceProvider` | 任务在哪执行、如何隔离 | git worktree | Docker、远程机器 |
| `Verifier` | 如何验收 | shell 命令 | LLM 审查、覆盖率门槛 |
| `Policy` | 失败如何处理 | 固定次数重试后升级 | 按预算、按错误类型 |
| `Pipeline` / `Stage` | 编排流程 | 固定流程（§8） | 自由编排、TDD、多轮审查 |
| `Tool` | 暴露给 agent 的 MCP 工具 | §9 列出的工具 | 任意新协作能力 |
| `Store` | 持久化 | SQLite | Postgres |
| `EventSink` | 事件订阅者 | API 事件流（CLI / GUI）、成本统计、日志 | Webhook、Slack |

## 6. 插件系统

### 6.1 统一 Agent 接口

插件负责把各家原生协议翻译为统一格式，core 不关心每家细节。

- **命令**：`start_session`、`send_input`、`interrupt`、`stop`、`resume`
- **事件**：`text_delta`、`tool_call`、`file_changed`、`turn_completed`、`usage`、`error`、`needs_approval`

### 6.2 能力声明

```toml
[capabilities]
streaming = true
interrupt = true
resume = true
model_switch = true
mcp = true          # 能挂 MCP → 协作工具经 MCP 注入
usage_report = true
```

core 根据能力调整行为。例如不支持 MCP 时，协作指令写进提示词，从输出中解析结构化结果。

### 6.3 三种来源（插件协议 = ACP）

M1 证明 ACP 足以表达统一 Agent 接口，因此**不再另定义 CrossAgents Plugin Protocol：插件协议就是 ACP**。
任何讲 ACP 的可执行程序（任何语言写的，例如用 TS 调官方 Claude Agent SDK）都能成为插件。

| 来源 | 说明 | 优先级 |
|---|---|---|
| 内置 | claude、codex，编译进 hub | 最低 |
| 插件目录 | `~/.crossagents/plugins/<name>/plugin.toml` | 中 |
| `config.toml` | `[plugins.<name>] command = [...]`，可覆盖内置的启动命令 | 最高 |

需要 ACP 表达不了的能力时，仍可为某个插件写原生驱动（`AgentPlugin` trait 不变），编排层不受影响。

### 6.4 插件清单

```toml
name = "gemini"
protocol = "acp"                        # 目前唯一支持的协议
command = ["gemini", "--experimental-acp"]   # 相对路径（含 / 或 \）相对插件目录解析
models = ["gemini-3-pro", "gemini-3-flash"]  # 展示与提示用
model_env = "GEMINI_MODEL"              # 可选：把 agent 的模型写入该环境变量
```

清单有误时跳过并记录警告，不影响 hub 启动。

### 6.5 协作工具的挂载

core 提供 `crossagents mcp`（每个 agent 带自己的令牌连回 hub）；**如何挂到 agent 上由插件决定**：
走 ACP 时在创建会话时传入 MCP server 列表即可（ACP 原生支持）；原生驱动下 Claude 用 `--mcp-config`，
Codex 用 `-c mcp_servers.*`；不支持 MCP 的走 §6.2 的提示词降级。

### 6.6 v1 驱动与内置插件：ACP 优先

v1 只实现一个 **ACP 驱动**（自写 ACP v1 最小线上类型 + 通用 JSON-RPC 层，见 [M1 任务书](tasks/M1.md)），通过现成的 ACP 适配器同时接入两家：

| 插件 | v1 driver | 以后可加的原生驱动 |
|---|---|---|
| claude | ACP → `claude-agent-acp`（基于官方 Claude Agent SDK，需 Node） | stdio-jsonl：`claude -p --input-format stream-json --output-format stream-json` |
| codex | ACP → `codex-acp`（Zed 维护，Rust） | json-rpc：`codex app-server` |
| gemini（v1 之后） | ACP（Gemini CLI 原生支持） | — |

插件此时主要负责：定位/安装适配器、模型与参数映射、能力声明、认证检查。
需要 ACP 表达不了的原生能力时，再为该插件补原生驱动，编排层不受影响。

## 7. 任务规格

弱模型的效果取决于规格质量，因此 `task_delegate` 只接受结构化规格，hub 校验：

| 字段 | 说明 |
|---|---|
| `goal` | 做什么、为什么 |
| `scope.allow` / `scope.deny` | 允许 / 禁止修改的文件或目录（同时用于并行冲突检测） |
| `interfaces` | 需要遵守的函数签名、数据结构、可调用的已有代码 |
| `acceptance` | 必须通过的命令，如 `cargo test auth::`、`cargo clippy -- -D warnings` |
| `references` | 开工前应阅读的文件 |

### 7.1 任务状态机

```
pending → running ⇄ waiting_answer
running → verifying → (passed → reviewing?) → merged
verifying(失败) → retrying → running        # 未超过重试上限
retrying(超限) → escalated → running         # 换更强模型或交回 Planner
任意状态 → failed | cancelled
```

## 8. v1 编排流程（固定 Pipeline）

```
1. Plan      Planner 理解需求 → 产出计划（任务列表 + 每个任务的规格 + 依赖关系）
2. Approve   【默认开启】人审批计划，可修改或打回；可在配置中关闭
3. Dispatch  按依赖和 scope 冲突排程，为每个任务创建 worktree 并启动新的 Worker 会话
4. Verify    Worker 调 task_complete 后，由 hub 自己执行 acceptance，不信 Worker 自述
5. Retry     失败：附错误输出打回同一 Worker；超过 N 次 → 升级（更强模型 / 交回 Planner）
6. Review    【可选】Reviewer 审查 diff
7. Merge     hub 合并分支；冲突交给 Planner
8. Summarize 每个任务以「摘要 + 完整 diff/日志文档」交回 Planner，Planner 汇总给人
```

**权限**：Worker 以全部放行（bypass）模式运行，风险靠 worktree 隔离 + 验收后才合并来控制。

**上下文经济**：Planner 只接收摘要，完整 diff/日志按需读取。规格不明时 Worker 用 `task_ask` 提问而不是猜。

**防失控**：委派深度上限、并发任务上限、单个 Run 的 token 预算上限。

## 9. 协作协议（MCP 工具）

| 工具 | 可用角色 | 作用 |
|---|---|---|
| `agent_list` | 全部 | 列出可见 agent 及状态 |
| `agent_send(to, text)` | 全部 | 发消息；目标空闲则作为新一轮输入，忙则排队到本轮结束 |
| `plan_submit(tasks)` | planner | 提交计划（进入 Approve 阶段） |
| `task_list()` / `task_status(task_id)` | planner | 查询任务（结果以消息推送，无需轮询等待） |
| `task_read(task_id)` | planner | 完整报告：规格、总结、验收输出、diff 统计、失败原因 |
| `task_ask(task_id, question)` / `task_answer(task_id, answer)` | worker / planner | 执行中的提问与回答 |
| `task_complete(task_id, summary)` / `task_fail(task_id, reason)` | worker | 交回结果或放弃 |
| `review_submit(task_id, approve, notes)` | reviewer | 审查结论；驳回意见转给 Worker |

工具定义与逻辑都在 hub（`GET /v1/me/tools`、`POST /v1/me/tools/{name}`），`crossagents mcp` 只是代理。

固定流程下，Planner 不直接调 `task_delegate`，而是通过 `plan_submit` 交给 Pipeline 调度；
`task_delegate` 留给以后的自由编排 Pipeline。

## 10. CLI

```
crossagents hub start|stop|status|run
crossagents plugin ls
crossagents agent add <name> --plugin claude --model haiku --role worker [--cwd <dir>]
crossagents agent ls|show|start|stop|cancel|rm <name>
crossagents run "<需求>" --planner <agent> --worker <agent> [--escalate <agent>] [--reviewer <agent>] [--no-approve]
crossagents run ls | show <id> [-v] | approve <id> | reject <id> "<原因>" | cancel <id>
crossagents send <agent> "<text>" [--wait]
crossagents attach <agent>
crossagents events [--since N] [--follow]
crossagents usage [--by role|model|run|agent]
```

所有命令支持 `--json`。Run id 可以用完整 id，也可以用分支名里的 8 位短 id。

## 11. 本地 API 与客户端

hub 对外只有一套本地 API，CLI 与 GUI 是平等的客户端，**所有业务逻辑都在 hub**（审批、取消、重试都是 API 调用）。

- **传输**：HTTP（命令与查询）+ SSE（事件流：任务树变化、agent 实时输出、状态、用量）。
- **版本**：路径带版本（`/v1/...`），事件带 schema 版本。
- **类型共享**：API 类型定义在 Rust 中，自动生成 TypeScript 类型（`ts-rs` 或 JSON Schema），供 GUI 直接使用。
- **认证**：只监听本机，客户端使用本地令牌（存于 `~/.crossagents/`）。

### 11.1 GUI（v2）

- **技术**：Tauri 2（Rust 后端 + Web 前端），参考 vibe-kanban 的 `tauri-app`。
- **形态**：GUI 作为 API 客户端连接 hub，不内嵌业务逻辑；前端为网页技术，以后可复用于远程/手机查看。
- **主要视图**：Run 列表与任务树、agent 实时输出、计划审批（可编辑规格）、diff 查看、成本面板。

## 12. 存储、安全与成本

- 状态存于 `~/.crossagents/state.db`（SQLite）：agents、runs、tasks、events（runs/tasks 为「索引列 + JSON」）。
- hub 只监听本机。人用 `~/.crossagents/token`；每个 agent 会话一个独立令牌，只能访问 `/v1/me/*`，
  工具调用可可靠识别调用者，并按角色限制可用工具。
- 成本与上下文用量从事件日志按需聚合（`GET /v1/usage`），只存数值，不存提示词与回复。
- 工作区：`~/.crossagents/worktrees/<run>/integration` 与 `.../<task>`；结果在 `crossagents/<run>` 分支，
  不碰用户当前工作区。

## 13. Crate 划分（实际）

```
crates/
├── core/              领域模型、统一 Agent 接口（AgentPlugin/AgentSession）、事件、Store、状态机
├── api-types/         本地 API 的请求/响应类型；生成 TS 类型到 bindings/
├── store-sqlite/      Store 实现（sqlx + 内嵌迁移）
├── drivers/           jsonrpc（通用 NDJSON JSON-RPC）、acp（ACP v1 client）
├── plugins/           插件注册表、内置 claude/codex、插件目录清单
├── orchestrator/      固定流程引擎、协作工具、计划校验与调度、git worktree、验收（只依赖 core）
├── hub/               组装与本地 API（axum）：runtime、agent 令牌、MCP 挂载、用量统计
├── cli/               crossagents 命令（含隐藏的 `crossagents mcp` stdio MCP 代理）
└── mock-agent/        可编程 ACP mock agent + 全部端到端测试（无需 LLM）
apps/
└── gui/               v2：Tauri 2 GUI
```

与最初设想的差异：没有独立的 `plugin-protocol`（协议就是 ACP）、`mcp`（MCP server 只是 CLI 里的薄代理，
工具逻辑在 orchestrator）、`workspace-git`（在 `orchestrator::workspace`，`WorkspaceProvider` trait 不变）。

主要依赖：tokio、serde、axum、sqlx（sqlite）、clap、reqwest、ts-rs。ACP 与 MCP 都自写最小实现，不依赖 SDK。

## 14. 里程碑

| 阶段 | 内容 | 状态 |
|---|---|---|
| M0 | workspace 骨架、core、hub + 本地 API（HTTP + SSE）、SQLite | ✅ [任务书](tasks/M0.md) |
| M1 | ACP 驱动、claude/codex 插件、agent 会话、send/attach | ✅ [任务书](tasks/M1.md) |
| M2–M3 | 协作工具（MCP）、任务规格、hub 验收、固定 Pipeline：审批、worktree 并行、重试/升级、Reviewer、合并 | ✅ [任务书](tasks/M2.md) |
| M4 | 插件目录（外部插件）、成本统计、收尾 | ✅ [任务书](tasks/M4.md) |
| v2 | Tauri GUI（任务树、实时输出、计划审批、diff、成本面板）；自由编排 Pipeline | 待做 |

v1 的自动化测试全部用 mock agent 完成；与真实 Claude Code / Codex 的联调需要用户本人登录后进行。

## 15. 已定决策

| 问题 | 决定 |
|---|---|
| 实现语言 | Rust |
| v1 接入的 agent | Claude Code、Codex |
| 接入方式 | v1 ACP 优先，一个驱动接两家；原生驱动按需补充 |
| 界面 | v1 CLI；v2 Tauri GUI；不做 TUI。API 从 v1 起按 GUI 标准设计 |
| 计划是否需人审批 | 默认需要，可关闭 |
| Worker 权限 | 全部放行 + worktree 隔离 + 验收后合并 |
| 编排方式 | v1 固定流程；自由编排作为后续 `Pipeline` 实现 |
| Worker 会话 | 每任务新会话；Planner 长期会话 |

## 16. 待研究

- 真实联调：`claude-agent-acp` / `codex-acp` 实际提供的模型选项名、bypass 模式名、`usage_update` 是否带成本。
  驱动对这些都做了降级处理（找不到就发 `agent.error` 警告并用默认值）。
- 计划审批在 CLI 下的编辑形式（目前只能批准或带理由驳回）。
- 合并冲突目前直接判任务失败；可以改为让 Worker 在新基线上重做。

## 17. 参考项目

| 项目 | 参考点 |
|---|---|
| [BloopAI/vibe-kanban](https://github.com/BloopAI/vibe-kanban)（Rust） | `crates/executors`（每家一个执行器、acp、mcp_config、model_selector）、`worktree-manager`、`tauri-app` |
| [agentclientprotocol/agent-client-protocol](https://github.com/agentclientprotocol/agent-client-protocol) | 统一 Agent 接口的蓝本；Rust crate |
| [zed-industries/codex-acp](https://github.com/zed-industries/codex-acp) | Codex 的 ACP 适配器 |
| [OrchestratorInc/agent-orchestrator](https://github.com/OrchestratorInc/agent-orchestrator) | 常驻 orchestrator + worker + worktree 的产品形态 |
| [Aider](https://github.com/Aider-AI/aider) architect/editor 模式 | 强弱模型分工的提示词与交接格式 |
| [cuibuaa/flow-crew](https://github.com/cuibuaa/flow-crew) | planner + 并行 coder + QA 关卡流程 |
| [aaif-goose/goose](https://github.com/aaif-goose/goose)（Rust） | 基于 MCP 的扩展系统、crate 组织 |
| [coder/agentapi](https://github.com/coder/agentapi) | PTY 兜底接入 |
| [senxiu-official/cutex](https://github.com/senxiu-official/cutex)（Rust） | Agent Bus、委派、项目隔离 |

**差异化**：按档位分工控制成本 + 规格即契约（hub 执行验收、失败自动升级）+ core/插件协议、CLI 优先可嵌入。
