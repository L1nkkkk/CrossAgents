# CrossAgents 设计文档（v2）

> 状态：草案 · 2026-10-21 · 取代 v1（v1 内容在 git 历史里，§15 列出两版差异）

## 1. 定位

CrossAgents 是一个多 agent 编码的「操作系统」。你和一个强模型的会话（Planner）对话，布置目标；
Planner 通过 hub 提供的工具开出若干便宜模型的会话（Worker）并行干活，检查它们的交付，来回打回返工，
直到目标达成。全程不需要你在模型之间传话，只在它卡住时找你。

用 OS 类比：

| OS | CrossAgents |
|---|---|
| 内核 | hub：调度、隔离、通信、资源限制；自身没有模型 |
| 进程 | Worker / Planner：一个会话就是一个进程，有身份、上下文、邮箱、状态 |
| CPU | 执行者：各家 agent 的运行时进程 + 模型 + 账号 |
| 系统调用 | hub 挂给会话的 MCP 工具 |
| IPC | 消息投递与唤起 |
| 文件系统命名空间 | 每个 Worker 一个固定的 git worktree |
| 资源限制 | 并发数、token 预算、轮数上限、账号额度 |
| init / shell | Planner：由人或强模型担任 |

各家 agent 通过驱动接入，体验优先：主力 agent 走原生接口（Codex 的 app-server、Claude Code 的 Agent SDK / stream-json），
能用到各家的完整能力（回退、分叉、恢复、结构化 diff、用量与限额）；其余 agent 走通用的
[ACP](https://agentclientprotocol.com) 驱动兜底，任何讲 ACP 的 agent 都能接入，不改核心。

### 1.1 目标

- 全程托管：布置一个目标，Planner 统筹，Worker 执行，Planner 检查，多轮迭代直到达成或触发停止条件。
- 会话是一等公民：Worker 长期存在，空闲挂起、来消息恢复，上下文不丢；会话之间可以来回通信。
- Planner 可以是人：人直接派任务、检查交付，走同一套工具和流程。
- 把手里几家订阅的额度用在对的地方：强模型只花在规划和检查上，编码交给便宜模型或另一家额度。
- 使用体验与各家原生客户端看齐：主力 agent 的回退、分叉、恢复、改动视图、用量都可用。
- GUI 是主入口；hub 是后端；CLI 是脚本和嵌入用的客户端。

### 1.2 非目标

- 上下文切换与跨厂商迁移：一个 Worker 一辈子绑定一个 Profile 和一个目录，不在执行者之间搬动。
- hub 自动升级（按失败次数换更强模型）：换人由 Planner 决定。
- 跨主机、多用户、设备配对；动态加载的二进制插件；TUI。

## 2. 架构原则

1. hub 不思考：所有判断在会话里（Planner / Worker / Reviewer），hub 只做确定性的事。判断错了查会话，规则错了查 hub。
2. 对话驱动：你和 Planner 会话对话；编排是 Planner 调用工具的结果，不是你对 hub 下的命令。
3. 依赖只朝内：`core` 只含领域模型、接口和事件；实现依赖 `core`。
4. 组件间用事件通信：会话状态、输出、任务变化、用量都是事件；GUI、统计、日志都是订阅者。
5. 配置驱动：角色用哪个 Profile、并发数、验收方式、预算都在配置里，分用户级和项目级两层。
6. 协议带版本：驱动对接的各家协议版本、MCP 工具、本地 API 的 schema 都带版本号。
7. 能力分级：hub 用自己的事件模型和能力表，不直接用任何一家的协议结构；驱动上报能力，GUI 和工具按能力开放功能，缺能力就降级，不报错。

## 3. 总体结构

```
┌──────────────────────── hub（内核，无模型） ────────────────────────┐
│ 本地 API（HTTP + SSE） │ 消息总线 │ 调度器 │ Worker 表 │ 工作区管理     │
│ 协作工具（MCP）        │ Store（SQLite）+ 事件流 │ 账号与用量         │
└───────────────────────────────┬────────────────────────────────────┘
                                │ Driver trait：open · prompt · interrupt · resume · fork · rollback
┌───────────────────────────────▼────────────────────────────────────┐
│ 驱动                                                                │
│   codex-native   → codex app-server（JSON-RPC over stdio，一进程多线程）│
│   claude-native  → claude -p stream-json 控制协议（一进程一会话）     │
│   acp-generic    → 任意 ACP agent（Gemini CLI、with-cli、Goose …）     │
│   每个会话 = 一个 Worker / Planner，各自的 cwd 与 MCP 列表            │
└────────────────────────────────────────────────────────────────────┘
      ▲ GUI / CLI 只通过本地 API 与 hub 交互；对话对象是会话，不是 hub
```

运行时示例：

```
hub
├── claude-native（Profile: opus）
│   └── 会话 planner      cwd = <项目>/.crossagents/local/integration/
├── claude-native（Profile: deepseek，经 Anthropic 兼容接口）
│   ├── 会话 worker-a     cwd = .../workers/a/     （挂起）
│   └── 会话 worker-b     cwd = .../workers/b/     （运行中，做 t5）
├── codex-native：一个 app-server 进程（Profile: codex）
│   └── 线程 worker-c     cwd = .../workers/c/     （空闲）
└── acp-generic（Profile: gemini）
    └── 会话 worker-d     cwd = .../workers/d/     （空闲）
```

## 4. 核心概念

| 概念 | 是什么 | 生命周期 |
|---|---|---|
| Profile | 驱动 + 模型 + 账号 + 环境变量，如 `opus`、`deepseek` | 长期，登记一次 |
| Worker | 一个会话：一段连续上下文 + 固定工作目录 + 邮箱 + 令牌 + 用量 | 直到被删或上下文满 |
| Planner | 也是一个会话，角色不同，工具集不同；可由人担任 | 同上 |
| Reviewer | 可选的审查会话 | 同上 |
| Project | 有 `.crossagents/` 的 git 仓库 | 长期 |
| Run | 一个目标下的一组任务、会话和消息；是 Planner 工作留下的轨迹，不是用户对 hub 的命令 | 目标达成或被停止 |
| Task | 派给某个 Worker 的一份结构化规格 | 完成、打回或取消 |
| Message | 会话之间或人与会话之间的一条消息，可关联回复 | 永久记录 |
| Event | 系统内一切变化的记录，持久化并广播 | 永久记录 |

### 4.1 会话（Worker / Planner）

会话的边界是上下文是否连续：来回传多少条消息、跨几个任务，都算同一个会话。任务只是派给会话的活，不决定它什么时候结束。

| 状态 | 含义 |
|---|---|
| running | 正在处理一轮 |
| idle | 本轮结束，进程还在，等消息 |
| suspended | 进程已释放，会话由 agent 自己的会话存储持久化；来消息时由驱动恢复（`resume`） |

- 空闲即挂起：idle 超过阈值（默认很短）转 suspended。驱动不具备 `resume` 能力的 Profile 不挂起，进程常驻。
- 回退与分叉：驱动具备能力时，可把会话回退到某一轮（可选同时撤销该轮之后的文件改动），或从某一轮分叉出新会话。分叉出的会话是新 Worker，用同一个 Profile，工作目录另建。
- 一个 Worker 绑定一个 Profile 和一个目录，不迁移。
- 会话只在两种情况下消失：被人或 Planner 显式删除（记录理由），或上下文满。上下文满时 hub 通知 Planner，由它决定是否新开一个接手。
- 会话与任务多对多：一个 Worker 可先后做多个任务；一个任务可换 Worker 接手（交接靠规格和打回记录）。
- 也允许不属于任何 Run 的独立会话（直接聊天、问代码）。

### 4.2 角色

| 角色 | 典型 Profile | 职责 | 工具集 |
|---|---|---|---|
| Planner | Opus / GPT-5 级，或人 | 理解目标、拆任务、开 Worker、派活、检查交付、决定下一轮 | `worker_*`、`task_*`、`ask_user`、`run_done` |
| Worker | Haiku / mini / DeepSeek 级 | 按规格编码；规格不明用 `task_ask` | `task_complete`、`task_fail`、`task_ask` |
| Reviewer | 中档或强模型 | 审 diff | `review_submit` |

全部会话都有 `agent_list`、`agent_send`。Worker 以放行模式运行，风险靠 worktree 隔离和合并前检查控制。

### 4.3 通讯录与可见范围

每个会话在 hub 有一张名片：名字、角色、Profile、所属项目与 Run、状态、用途说明（Planner 开 Worker 时填写）、当前任务。

名片还带一份人设（`AgentProfile`，可选）：显示名（如 Ada）、头衔（设计师、安全审核专员、Coder…）、头像（emoji 或字母）、颜色、人设说明。
角色（planner / worker / reviewer）仍只决定工具集；头衔和人设让同一角色下分出不同的「人」。人设说明写进该会话的开场说明（`You are Ada, the team's 设计师.` 加人设正文），
改动在下次开会话时生效；Worker 克隆继承模板的整张名片。接口：`POST /v1/agents` 的 `profile`、`POST /v1/agents/{key}/profile`（整体替换，发 `agent.updated`）；CLI `agent add --display-name/--title/--avatar/--color/--persona`。

- 启动时注入：开场说明里写清自己是谁、Planner 是谁、同组还有谁、怎么通信。
- 随时可查：`agent_list` 返回可见范围内的名片。
- 变化通知：有会话加入或删除时，hub 给相关会话发系统消息。

可见范围决定「能看到谁、能给谁发消息」，可配置：

| 级别 | 含义 |
|---|---|
| `run` | 只看到同一 Run 的会话 |
| `project` | 同一项目的会话（默认） |
| `global` | hub 上所有会话 |

- 项目级 `config.toml` 设默认值，单个会话可覆盖（如把某个 Worker 设为 `global`，供其他项目咨询）。
- 可另加 `allow` / `deny` 名单做细粒度控制。
- 可见是双向检查：发送方能看到对方，且对方的范围也允许被这个发送方看到，消息才投递。

### 4.4 工作区边界

可见范围只管通信，不管文件。会话能动的文件只有自己的工作目录：

- Worker：`.crossagents/local/workers/<name>/`；Planner：集成分支目录。
- 跨项目通信时，被询问的 Worker 仍在自己项目的目录里干活，改动进自己项目的集成分支；它可以回答问题、按请求在本项目里改代码，但碰不到发送方项目的文件。
- 这条边界由各家 agent 的权限机制执行，强度不同：Codex 的 `workspace-write` 沙箱真正限制写入范围（Windows 上要先装好 Codex 的 Windows 沙箱，否则连 cwd 内都写不了）；Claude Code 的放行模式只以 cwd 为工作目录，并不阻止访问目录外的路径。需要硬隔离的 Profile 应选用带沙箱的模式，或以后用 Docker 类 `WorkspaceProvider`。
- 任何改动最终只经集成分支、由人合并进主分支，这是不依赖 agent 自觉的最后一道。

### 4.5 邮箱与投递

每个会话一个邮箱，`agent_send` 的消息先持久化再投递：

| 对方状态 | 处理 |
|---|---|
| idle | 立即作为新一轮输入唤起 |
| running | 排队，本轮结束后投递 |
| suspended | 驱动 `resume` 恢复后投递 |

- 投递内容带信头：发件人、消息 id、`reply_to`；GUI 按线程显示。
- 默认异步：发完继续，回复作为新一轮唤起发送方。`wait=true` 为同步，hub 设超时上限。
- 保护：检测等待环（A 等 B 且 B 等 A 时拒绝后者）；单个 Run 的来回轮次有上限；所有消息进事件流。

### 4.6 托管会话与接入会话

| 类型 | 来源 | hub 能否唤起 | 收消息方式 |
|---|---|---|---|
| 托管会话 | hub 通过驱动开启（原生或 ACP） | 能 | 消息作为新一轮推送 |
| 接入会话 | 外部 agent（终端里的 Claude Code、Codex、CodeBuddy 等）自己连入 | 不能 | 自己拉取 |

接入方式：`crossagents session attach-token --name <name> --role <role>` 登记并取得令牌，在外部 agent 的 MCP 配置里加上
`crossagents mcp`（环境变量 `CROSSAGENTS_URL`、`CROSSAGENTS_AGENT_TOKEN`）。接入会话可以担任任何角色，包括 planner，
不另设预算；它出现在通讯录里，遵守同样的可见范围。

- 因为 hub 唤不起它，接入会话额外有 `agent_call`（发送并同步等回复）和 `inbox_wait`（阻塞等新消息）。
- 外部 agent 只在自己的一轮里调工具时才能收到消息；需要全程托管时 Planner 仍应是托管会话。接入会话适合「在自己的 agent 里顺手把活派给 hub」。
- 以后可加：hub 作为 ACP 服务端，让 Zed 等 ACP 客户端直接与 hub 里的会话对话；本地 HTTP API 供脚本调用。

## 5. hub 的职责

hub 是内核，不是对话对象。它做的都是让会话「能活、能通信、能动手」的事：

- 投递与唤起：把消息送到会话；对方挂起则先恢复，忙则排队到本轮结束。
- 寻址：会话之间按名字互相找到。
- 系统调用：以 MCP 工具暴露开 Worker、派任务、查状态、合并分支等能力，逻辑都在 hub。
- 资源：工作目录、集成分支、账号额度、并发数、预算与停止条件。
- 记录与观测：会话、消息、任务、事件全部持久化；GUI 从这里读实时输出与历史。

留在 hub 手里、不交给会话的：停止条件与预算的执行、worktree 隔离、向用户主分支合并的最后一道。

## 6. 托管流程

```
你 ──对话──▶ Planner 会话
                │ 拆任务，必要时把计划发回给你（自然语言审批，第一轮默认需要）
                │ worker_spawn / task_assign
                ▼
      hub：同步 Worker 目录到集成分支，投递任务
                │ Worker 编码 → task_complete
                ▼
      hub：可选的命令初筛（编译 / 测试，失败直接打回 Worker）
                │ 通过
                ▼
      唤起 Planner：交付摘要 + diff stat（完整 diff 按需读）
                │ task_accept ──▶ hub 合并到集成分支 crossagents/<run>
                │ task_reject ──▶ 同一个 Worker 会话返工（它记得之前的上下文）
                ▼
      Planner 读本轮结果 → 继续派下一批 / 向你提问 / run_done
                │ run_done
                ▼
      hub 跑目标级验收命令（如有）→ 通知你：结果在集成分支，等你合并
```

- 验收以 Planner 检查为主；命令验收是可选初筛，由 Planner 在规格里写或项目配置里给默认值。
- 重试多次不行，由 Planner 决定开一个更强 Profile 的新 Worker 接手，hub 不自动升级。
- 停止条件由 hub 执行：轮数上限、token / 额度预算、连续失败次数，触发即停并通知你。
- Planner 只在例外时通过 `ask_user` 找你，hub 经 GUI 推送（以后可接 IM）。
- 多轮接力：Planner 在集成分支维护 `.crossagents/progress.md`（目标、已完成、遗留、下一步）。
- 合并冲突：hub 不自动解决，通知 Planner，由它派 Worker 在新基线上重做。

## 7. 任务规格

弱模型的效果取决于规格质量，`task_assign` 只接受结构化规格，hub 校验：

| 字段 | 说明 |
|---|---|
| `goal` | 做什么、为什么 |
| `scope.allow` / `scope.deny` | 允许 / 禁止修改的路径（同时用于并行冲突检测） |
| `interfaces` | 需遵守的签名、数据结构、可调用的已有代码 |
| `acceptance` | 可选：初筛命令 |
| `references` | 开工前应阅读的文件 |
| `assignee` / `reviewer` | 可选：按专长指定的队友（名字）。hub 校验 assignee 是团队里的 worker、reviewer 是团队里的 reviewer；不写则用 run 的默认 Worker / Reviewer |

**按专长派活**：每个 run 有一个团队（`RunConfig.team`）——默认是 Planner 仓库里登记的全部 worker 和 reviewer（Worker 克隆在 worktree 里，自然不算），也可在发起时指定。
Planner 的开场说明列出团队：名字、显示名与头衔、模型、人设的第一句，并标出默认 Worker；它在 `plan_submit` 里给每个任务写 `assignee`（和可选的 `reviewer`）。
派活时 Worker 从被指定的队友克隆（继承它的模型和名片），失败升级仍走 `escalate`。

任务状态机：

```
pending → running ⇄ waiting_answer
running → delivered → (checking by planner) → accepted → merged
delivered → rejected → running          # 同一 Worker 返工
任意状态 → failed | cancelled            # Planner 或停止条件
```

## 8. 协作工具（MCP）

| 工具 | 角色 | 作用 |
|---|---|---|
| `agent_list` | 全部 | 列出可见范围内的会话名片（§4.3） |
| `agent_send(to, text, reply_to?, wait?)` | 全部 | 发消息；`reply_to` 关联回复；`wait` 则挂起本轮等回复 |
| `agent_call(to, text, timeout)` / `inbox_wait(timeout)` | 接入会话 | 同步等回复 / 阻塞等新消息（§4.6） |
| `worker_spawn(profile, name)` / `worker_remove(name, reason)` | planner | 开 / 删 Worker 会话 |
| `task_assign(worker, spec)` | planner | 派任务（新开或复用 Worker） |
| `task_list()` / `task_read(task_id)` | planner | 查任务；完整报告含规格、总结、初筛输出、diff |
| `task_accept(task_id)` / `task_reject(task_id, notes)` | planner | 检查结论；驳回意见投递给 Worker |
| `task_answer(task_id, answer)` | planner | 回答 Worker 提问 |
| `ask_user(question)` | planner | 向人提问，经 GUI 推送 |
| `run_done(summary)` | planner | 宣布目标达成，触发目标级验收 |
| `task_complete(task_id, summary)` / `task_fail` / `task_ask` | worker | 交付、放弃、提问 |
| `review_submit(task_id, approve, notes)` | reviewer | 审查结论 |

人当 Planner 时，这些工具以 GUI 操作和 CLI 命令的形式提供，走同一条管线。

## 9. 驱动

体验优先：主力 agent 走原生接口，其余走 ACP。三个驱动实现同一个 `Driver` trait，hub 只认这个 trait 和自己的事件模型。

### 9.1 驱动一览

| 驱动 | 对接 | 进程模型 | 用途 |
|---|---|---|---|
| `codex-native` | `codex app-server`（JSON-RPC over stdio） | 每个 Profile 一个进程，进程内多个线程（会话） | Codex 主力 |
| `claude-native` | `claude -p --input-format stream-json --output-format stream-json --permission-prompt-tool stdio`，hub 直接说控制协议（即 Agent SDK 所用的协议，见 [实测](research/claude-stream-json.md)） | 一进程一会话 | Claude Code 主力，含经兼容接口接入的 DeepSeek 等 |
| `acp-generic` | 任意 ACP agent | 一进程多会话 | Gemini CLI、with-cli（`acp` 子命令）、Goose 等长尾 |

Claude 侧走侧车还是 stream-json 由实测决定（§14）：侧车能拿到 SDK 的全部能力（权限回调、hooks、文件检查点），stream-json 少一个进程但能力受 CLI 参数限制。

### 9.2 Driver trait 与能力

```
Driver（一个 Profile 一个实例）
  open(spec) -> Session                    spec：会话记录（模型、cwd、config）、MCP 列表、开场说明、权限策略或回调、环境变量、事件通道
  resume(session_id, spec) -> Session      session_id 即会话开启时上报的厂商 id（Codex 线程 id）
  capabilities() -> { streaming, interrupt, mcp, multi_session, resume, fork, rollback, revert_files,
                      set_model, structured_diff, usage, rate_limits, permission_callback, compaction }
Session（一个会话）
  prompt(text) -> { stop_reason, turn_id } / interrupt()
  suspend() / shutdown()
  fork(at_turn?, spec) -> Session / rollback(before_turn, revert_files)
  set_model(model?, effort?) / compact()
```

- 驱动启动时上报能力；hub 和 GUI 按能力开放功能（如没有 `rollback` 就不显示回退按钮），缺能力降级而不报错；驱动对不具备的操作返回「不支持」。
- 驱动把各家事件翻译成 hub 的统一事件：文本流、思考、工具调用、文件改动（按轮）、轮次开始 / 结束、用量、限额、权限请求、错误、进程退出。原生驱动拿到的细节不在这一层丢掉，统一事件留有 `raw` 扩展字段。
- 权限：Worker 默认放行；需要审批的操作经驱动的权限回调交给 hub，hub 按规则自动决定或转给 Planner / 人。
- 相对最初草稿的调整（依据 [codex app-server 实测](research/codex-app-server.md)，见 [N1 任务书](tasks/N1.md) §4）：拆成 Profile 级与会话级两层，进程池归 Profile；
  `resume` 以会话 id 为参数；`rollback` 去掉「指定轮及之后」，与上游一致；能力表补 `streaming`、`interrupt`、`mcp`、`multi_session`；会话多 `compact`。

### 9.3 配置与挂载

- Profile 指定驱动：`driver = "codex-native" | "claude-native" | "acp-generic"`，再加模型、账号、`env`（如 Claude Code 经 `ANTHROPIC_BASE_URL` 接 DeepSeek）。
- ACP 插件来源与优先级不变：内置 < 插件目录 `~/.crossagents/plugins/<name>/plugin.toml` < `config.toml` 覆盖。
- 协作工具挂载：开会话时把 `crossagents mcp`（带该会话的令牌）加入它的 MCP 列表；原生驱动用各家的 MCP 配置方式传入。
- 账号隔离沿用 M5：每个账号一个独立的 `CODEX_HOME` / Claude 配置目录，凭据不出 hub 所在机器。

### 9.4 跟上游

原生接口跟着上游快速变化，代价是接口变动可能让驱动静默失效。对策：

- 每个原生驱动一套冒烟测试：开会话、发消息、权限请求、中断、挂起恢复、回退、分叉、用量上报。升级 Codex / Claude Code 版本前后各跑一遍。
- hub 记录每个驱动对接的上游版本；版本不在已验证范围时给出警告，仍可运行。
- 原生驱动坏了时，同一 Profile 可临时切到 `acp-generic`（`codex-acp`、`claude-agent-acp`）兜底，功能按能力降级。

## 10. 工作区与目录

```
~/.crossagents/                      用户级
├── config.toml                      端口、Profile 定义（含驱动）、插件覆盖
├── state.db  token  hub.json  logs/
├── accounts/                        各家账号目录
└── plugins/                         acp-generic 用的外部 ACP 插件

<仓库>/.crossagents/                 项目级
├── config.toml                      角色用哪个 Profile、并行数、验收方式、预算   （提交）
├── AGENTS.md                        给 Worker 的项目说明                          （提交）
├── PLANNER.md                       给 Planner 的额外说明，可选                   （提交）
├── progress.md                      Planner 维护的目标与进度，多轮接力             （提交到集成分支）
└── local/                           本机状态，gitignore
    ├── workers.json                 Worker ↔ 会话 id ↔ 目录
    ├── integration/                 集成分支 crossagents/<run> 的 worktree，Planner 在这里
    └── workers/<name>/              每个 Worker 的固定 worktree
```

- Worker 目录固定不换（Claude Code 的会话与目录绑定）。派新活前 hub 把该目录同步到最新集成分支。
- 创建 Worker 目录时复制主目录未提交的本地设置（如 `.claude/settings.local.json`），保证行为一致。
- 用户当前工作区不被触碰；结果在集成分支上，最终是否合入主分支由人决定。

## 11. 客户端

hub 对外只有一套本地 API（HTTP + SSE，`/v1/`，本机令牌），CLI 与 GUI 是平等客户端，业务逻辑全在 hub。

### 11.1 GUI（主入口）

- 主区域：与某个会话的对话窗口（默认 Planner）；计划审批、提问回答都在对话里。
- 侧边：按 Run 分组的会话列表与状态（Run 下是它的 Planner、Worker、Reviewer），其下是独立会话；任务进度、diff、用量从侧边进入。
- 右侧抽屉按需打开：任务详情（规格、总结、验收、diff stat），以后加完整 diff 与事件流。
- 会话操作按驱动能力显示：回退到某一轮（可选撤销文件）、从某一轮分叉、按轮查看改动、中途换模型。
- 设置页：Profile、账号、项目配置。
- 视觉：暖灰中性色 + 唯一强调色靛蓝（indigo），颜色只表达状态、diff 和警告；agent 正文用衬线体（中文回落到 UI 字体），界面用无衬线体；
  工具调用折叠成单行等宽条目；深浅色跟随系统。界面文字中英混排（术语保留英文：run、Planner、Worker、Approve 等）。
- 对话数据：`GET /v1/agents/{key}/transcript` 返回按轮分组的历史（hub 由事件折叠而成）和驱动能力，客户端再折叠之后的实时事件；
  `GET /v1/events` 可加 `agent=<名字或 id>` 只看某个会话。
- 消息来源：`agent.input` 和每一轮都带 `from`：`human`、`orchestrator { run_id, task_id? }`、`agent { agent_id }`。只有人的消息显示为右侧气泡，其余是一行可展开的消息。
- 排队：忙时到达的消息排在会话的收件箱里，可以撤回（`DELETE /v1/agents/{key}/messages/{message_id}`，事件 `agent.input_withdrawn`）；
  `POST .../messages` 带 `now: true` 时插到最前面并中断正在进行的一轮。
- 状态统一写成「符号 + 一个词」，并归到 需要你 / 等待 / 进行中 / 已结束 四组（`apps/gui/src/lib/taskState.ts`）。
- 技术与安全设定沿用 [V2 任务书](tasks/V2-GUI.md)（Electron + React，令牌只在主进程）；改版见 [V3](tasks/V3-GUI.md)、[V4](tasks/V4-GUI.md)、[V5](tasks/V5-GUI.md) 任务书；
  mock 预览、视觉层次、按轮 diff 与右侧工具列见 [V6](tasks/V6-GUI.md)。

### 11.2 CLI

```
crossagents hub start|stop|status
crossagents profile add|ls|rm
crossagents account ...                       # 沿用 M5
crossagents session ls|spawn|rm|attach
crossagents session attach-token --name <n> --role <r>   # 登记接入会话，给外部 agent 用
crossagents send <session> "<text>" [--wait]  # run "目标" 是 send planner 的快捷方式
crossagents task ls|show|accept|reject        # 人当 Planner 时用
crossagents events [--follow] | usage [--by ...]
```

## 12. 存储、安全与成本

- SQLite：profiles、sessions、runs、tasks、messages、events。
- hub 只监听本机；人用 `~/.crossagents/token`；每个会话一个令牌，只能访问 `/v1/me/*`，按角色限制工具。
- 用量按会话记录，可按 role / profile / run / account 聚合；额度窗口与重置时间沿用 M5 账号能力。

## 13. Crate 划分

沿用 v1：`core`、`api-types`、`store-sqlite`、`plugins`、`hub`、`cli`、`mock-agent`；`apps/gui`。变化：

- `core`：新增 `Driver` trait、能力表、统一会话事件。
- `drivers`：拆为 `codex_native`（已完成，见 [N1](tasks/N1.md)）、`claude_native`（已完成，见 [N3](tasks/N3.md)）、`acp`（即 `acp-generic`，以后改为一进程多会话）三个模块；`claude_native` 直接说 stream-json 控制协议，不需要侧车。
- `orchestrator`：固定 Pipeline 改为工具驱动的编排 + 停止条件。
- `mock-agent`：同时模拟 ACP 与 Codex app-server 两种协议，供测试使用。

## 14. 待实测

- ~~`codex app-server` 的实际方法与事件~~：已实测（codex-cli 0.158.0），见 [codex-app-server.md](research/codex-app-server.md)。线程开启、恢复、分叉、回退历史、按轮 diff、用量与限额都支持；回退不撤销文件；单进程承载上限未压测。
- ~~Claude 侧：Agent SDK 侧车与 stream-json 两条路各自能拿到哪些能力~~：已实测（Claude Code 2.1.304），见 [claude-stream-json.md](research/claude-stream-json.md)。SDK 就是 stream-json 控制协议的封装，`claude-native` 由 Rust 直接说这套协议，不用侧车；恢复、分叉、文件检查点与回退、权限回调、额度都支持。
- ~~Claude 会话是否与目录绑定~~：不绑定，换目录 `--resume` 照样记得对话。
- DeepSeek 的 Anthropic 兼容接口能否直接被 Claude Code 使用。
- 各驱动在放行模式下对 cwd 外文件的实际限制（决定 §4.4 的边界强度）。Codex 已测：`danger-full-access` 不限制，`workspace-write` 限制在 cwd 内。
- ACP 兜底：`claude-agent-acp`、`codex-acp`、with-cli `acp` 是否支持 `session/load`。

## 15. v1 到 v2 的差异

| 方面 | v1 | v2 |
|---|---|---|
| 入口 | `crossagents run` 对 hub 下命令 | 与 Planner 会话对话，Planner 用工具编排 |
| 最小单位 | Agent（插件 + 模型 + 角色） | Profile（驱动 + 模型 + 账号）+ 会话（Worker / Planner） |
| Worker 生命周期 | 每任务新建、用完即弃 | 长期存在，空闲挂起，来消息恢复 |
| 驱动 | 只有 ACP，一进程一会话 | Codex / Claude 原生驱动为主，ACP 兜底；按能力开放回退、分叉、恢复 |
| 验收 | hub 跑命令，退出码定胜负 | Planner 检查为主，命令为可选初筛 |
| 升级 | hub 按次数自动换强模型 | Planner 决定换人 |
| 流程 | 固定一轮 | 多轮托管，直到 `run_done` 或停止条件 |
| 通信 | `agent_send` 发完不管 | 可关联回复、可等待回复 |
| 工作区 | 按任务的临时 worktree | 按 Worker 的固定目录 + 项目级 `.crossagents/` |
| 审批 | hub 的批准 / 驳回按钮 | 对话里自然语言审批，第一轮默认需要 |

## 16. 参考项目

沿用 v1 §17：vibe-kanban、agent-client-protocol、codex-acp、agent-orchestrator、Aider architect/editor、flow-crew、goose、agentapi、cutex。

差异化：会话即进程的 OS 式模型 + 对话驱动的全程托管 + 规格即契约 + 主力 agent 原生体验、长尾 agent 经 ACP 接入 + CLI 可嵌入。
Cutex 的会话总线、委托、项目与 Director、配对等设计是主要参照；与它不同，CrossAgents 不维护任何一家 agent 的分支，直接对接上游官方接口。
