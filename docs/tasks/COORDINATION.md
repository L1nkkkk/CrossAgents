# 省协调成本：通知分类、记账、评审闭环与会话交接

> 日期：2026-11-09 · run `c917a9ab` · 状态：九个任务的代码已合并（`d0fe82d`），**已随 0.2.12 生效**（2026-11-09 发版 `cdfe86b`，本机 hub 已在跑 0.2.12）。
> 历史：这一轮自己是在 **hub 0.2.11** 上跑的：仍按 0.2.11 的流程走（每条通知立刻叫醒、lead 手动开评审、手动接受，没有任务预算和结构化交接），新功能没有在跑这一轮的 hub 上启用。人当时在 Mac 上测远程 GUI，连的就是这台机器上正在跑的 0.2.11；本轮没有停、重启或升级它，也没有改 `~/.crossagents` 下的文件。测试要起 hub 就用临时 home 和端口。
>
> 依据：run 笔记 `brief/common`（共同约定与数据）、`lead/replay`（回放结果，主导核对过）、`lead/phase-model`（换档实测与决定）、`lead/leftovers`（遗留），以及 #usage / #notices / #cursor-driver / #rounds / #review-loop / #lead-session / #task-budget / #replay / #phase-model 九个任务的交付；代码以合并后的 run 分支为准。
> 完成清单见 [DONE.md §二十八](../DONE.md)，未完成与待实测见 [TODO.md §二十八](../TODO.md) 与本文 §11；Cursor CLI 的协议背景见 [cursor-cli 记录](../research/cursor-cli.md)，价目表格式见 [prices.md](../prices.md)，前一轮「减少额度消耗」见 [USAGE-TRIM.md](../USAGE-TRIM.md)。
>
> 数字口径：§1 的占比来自 `brief/common` 对 2026-11-07～08 日志的统计；§8 的回放数字来自 `lead/replay`（主导接受前重跑核对过，全部是估算）；§6 与 §9 的实测来自 #cursor-driver / #phase-model 的交付，都是那一次本机实测，不是普遍结论。估算与账单、实测与设计确认各自标清，不混用。

## 1. 为什么做

lead（强模型，不换）的 token 大多花在被叫醒、看汇报、来回返工上。`brief/common` 对 2026-11-07～08 日志的统计：

| 指标 | 值 |
|---|---|
| lead 被叫醒 | 125 次 |
| 输入 | 1.55 亿 token（95% 命中缓存） |
| 输出 | 139 万 token |
| 规划占输入 | 19% |
| 跟人对话 | 12% |
| 只报信的通知 | 17% |
| 队友交付后 lead 自己查 | 约 32% |
| 评审和返工来回 | 约 21% |

目标：**让 hub 掌握流程和事实，lead 只处理需要判断的决定**。做法是把「记录」与「叫醒」拆开（§2）、把流程和事实记进可查的账（§3、§4、§5）、让驱动少走一次模型请求（§6）、每轮给 lead 一个干净的会话（§7），并用旧日志回放估算能省多少（§8）、实测换档的代价（§9）。

一条硬约束：**不要为了维持缓存而制造唤醒**（`brief/common`）。省下多少以实测为准，不预设比例，也不从 token 推算订阅额度（承 [USAGE-TRIM.md](../USAGE-TRIM.md) §1 的原则）。

> 注意：本节 125 次 / 1.55 亿是 `brief/common` 的统计口径，与 §8 回放脚本对同一批日志重建出的 165 次 / 197.3M 不是同一个口径（脚本按 `agent.input` / `agent.usage` 增量重建回合，还包含这一轮自己的回合）。两处数字各按各的来源写，不互相换算。

## 2. 通知分类与合并窗口

`crossagents_core::NoticeClass`（`crates/core/src/notify.rs`，serde `snake_case`，见 `bindings/NoticeClass.ts`）：

| 类 | 含义 | 叫醒方式 |
|---|---|---|
| `Silent` | 已派出、进度、自动返工中 | 只记录，跟着下一次唤醒带过去，不单独叫醒 |
| `Batch` | 通过检查和评审、等 lead 接受，且没有下游依赖 | 合并：最后一条之后静默 **60 秒**，或从第一条起最多 **180 秒**，先到为准 |
| `Unblock` | 等接受，且接受后会解锁下游任务 | 立刻叫醒，跳过窗口 |
| `Decide` | 要 lead 决定的问题、不能自动恢复的失败、争议或返工超限、预算超限，以及主人自己设的提醒和 watch | 立刻叫醒 |
| `RunDone` | 全部任务结束 | 立刻叫醒，做整轮验收 |

- **记录与叫醒分开**：每条通知先落库，再按类决定叫醒。`Host::notify` / `Runtime::notify` 带 `class` 参数，orchestrator 与 hub 的调用点按上表标类；拿不准的标 `Decide`，队友进度（`watch.rs` 的 lead-progress）一律 `Silent`——这一轮就是要改掉「有进度就叫醒 lead」的旧规则。
- **落库**：新表 `pending_notices`（迁移 `0017_pending_notices.sql`；列 `agent`、`topic`、`line`、`class`、`first_at`、`updated_at`、`carried_by`、`wake_tries`；同一 topic 新行替换旧行、`first_at` 不重置，窗口不从头算）。行在带着它的回合成功结束后删除；叫醒失败或被打断最多再自动叫醒一次，到 `wake_tries = 2` 就只跟下一次唤醒走。**hub 重启不丢待办，到期照样叫醒**。
- **叫醒消息格式**：先列「要处理的」（`Unblock → Decide → RunDone → Batch`，按 `action_rank`），再列「自上次以来的变化」（`Silent`），每条标分类。人的消息照旧立刻处理，并带上所有待带的通知（沿用 `take_waiting_digest`）。
- **窗口可配**：`[hub] notice_batch_quiet_secs`（默认 60）、`notice_batch_max_secs`（默认 180），见 `crates/hub/src/paths.rs`。
- **事件**：`notice.recorded`（带 topic / line / class）供回放和回合记账用；`notice.carried` 记一条消息带走了哪些通知。
- 已归档 agent 的待带通知不投递、也不清理（同一个 agent 恢复后还能看到），删除 agent 时才清——故意如此，见 §11.2。

## 3. 记账口径（用量、三层、回合）

**三层**，各自记什么（`brief/common`）：

| 层 | 记什么 |
|---|---|
| 事件 | 谁产生了什么、为什么 |
| lead 的一次工作回合 | 哪些事件触发、有没有做决定 |
| 模型请求 | 次数和各自的用量；拿不到就标未知 |

- **原始 token 照实全记**：输入、缓存读、写缓存、输出。`TokenUsage`（`crates/core/src/driver.rs`、`bindings/TokenUsage.ts`）加 `cache_write_input` 和 `requests: Option<u64>`（会话累计的模型请求次数，`None` 表示不知道）；`cached_input` 和 `cache_write_input` 都是 `input` 的子集，`input` 含义不变，旧事件反序列化得 `0` / `None`。
- **各驱动只填自己真知道的**：Cursor 的写缓存、Codex 的 `cacheWriteInputTokens`、Claude 的 cache creation；`requests` 只在驱动真能数出来时填（With 的 `STEP_FINISHED`/`call_llm`、Codex 每次的 `last`、Claude 的 `iterations`），Cursor 数不出来就 `None`。**供应商没给的细分（例如 Cursor 单次请求的用量）标未知，不编**。
- **估算与账单分开**：两种费用分两列——agent 报的 `cost_usd`（当账单看）与按价目表估的（标「估算」）。价目表是可选的 `<home>/prices.toml`，按模型名写 `input` / `cached_input` / `cache_write_input` / `output` 每百万 token 的美元价，**不内置任何价格**（格式见 [prices.md](../prices.md)）。没有价格就写「未知」，不混成一个「打折 token」。
- **用量只在回合结束时上报**，预算可能超出一个回合，**不说成精确硬上限**。
- **回合记录（#rounds）**：每个回合结束发 `agent.round`（触发消息 id 与来源、带上的通知及分类、这回合的用量增量、用过的 hub 工具、`decided`、`prompt_via_file`；拿不到的字段写 `null`）；工具分发处发 `agent.tool_used { id, tool, ok }`；Cursor / With 把提示词写进文件时发 `agent.prompt_via_file`。哪些工具算「做了决定」由 `hub::rounds::decides()` 单独列：改任务 / run 状态、发消息、问人、提交计划、落地、`note_write`、`lesson_*`、`branch_sync` 算；`workspace_run`、`checks_run` 和读、进展、核对不算。
- **查看**：`crossagents usage --by round [--agent <名字或 id>] [--run <id>]` 列出回合，汇总回合数、没做决定的回合数与它们的 token、因读消息文件多出请求的回合数。回放脚本（§8）有 `agent.round` / `notice.recorded` 时按原值用，没有才推断。

## 4. 评审闭环与版本绑定

流程：**交付 → 自动检查 → agent 评审 → 等接受 → 合并**。检查或评审不过就回原 worker；超过返工上限、预算上限或需求有争议，才交给 lead。这一轮接受仍由 lead 点。

- **自动开评审**：检查通过后，任务配了评审人（`spec.reviewer` 或 run 的 `reviewer`）就由 hub 自己开评审，lead 只收到一条 `Silent`；没配评审人就直接发「等接受」通知，不再转整份汇报，lead 要细节用 `task_get`。
- **自动退回**：评审要求修改时退回原 worker（同一执行会话、`attempts` 加一、清掉旧检查结果），结构化意见作为它下一回合的消息，lead 只收到一条 `Silent`。退回次数到上限（默认 3，`spec` 可改）或 worker 对某条意见答复有争议，才交给 lead（`Decide`）。原 worker 会话已不在的，新开执行会话接手并在通知里写明。`on_reject` 指向别人的旧路径保留。
- **结构化意见**：`ReviewFinding` 加 `id`（hub 按任务编 F1、F2…，跨轮次不变）、`blocking`（默认 P0/P1 为 true）、`basis`（依据）、`verify`（怎么验证修好了）；`task_complete` 加可选的 `answers: [{ id, status: fixed|not_fixed|disputed, note }]`，下一轮评审把答复给评审人看。
- **版本绑定**：开评审记下 `verified_sha` 和轮次；`review_submit` 必须带 `reviewed_sha`（缺就拒绝），对不上是旧评审或重复提交就拒绝且**不改状态**。评审过的任务，普通 `task_accept` 要求 `verdict == Merge` 且 `reviewed_sha == verified_sha`（`force` 加理由仍可）。同提交判断去空白、不分大小写、至少 7 位前缀算同一个提交，两条拒绝信息都给完整提交号（#task-budget `2201c36` 顺手修的短提交号问题，见 §11.4）。**检查和评审只证明它们看过的那个提交：worker 改过以后旧结论作废，迟到或重复的评审不能推动状态**。
- **等接受**：有下游依赖或 `next` 路由的任务发 `Unblock`，其余发 `Batch`；通知写成决策材料（哪些任务等接受、接受哪个会解锁什么、有哪些非阻塞意见已登记、要你决定什么）。下游还依赖别的未合并任务时，`Unblock` 文字写「#x 还在等 #y」，不谎称解锁。`task_accept` 支持 `tasks: [...]` 批量，逐个返回结果。
- **通知结清与重启**：等接受通知在离开该状态的每条路径结清（合并、`task_rework`、`task_drop`、Passed 又交新汇报、开评审、人退回）。hub 重启后 `Reviewing` 重发评审说明或补 `Decide`（评审人会话不在时），`Passed` 补「等接受」通知。
- **协议文本**（`crates/orchestrator/src/protocol.rs`）：planner 默认靠检查和评审拿证据，遇到关键架构决定、证据矛盾或摘要不够才定点读代码；说明自动评审、批量接受和通知分类；互不依赖的工具调用放在同一条消息里并行调。

## 5. 任务预算

`spec.budget`（`TaskSpec`）逐字段覆盖 `RunPolicy.task_budget`（run 默认，`Option`，**缺省 = 无上限，整套检查跳过**）。字段（`bindings/TaskBudget.ts`）：`max_usd`、`max_tokens`、`max_requests`、`max_rework`、`max_minutes`、`max_idle_minutes`。

- **归账不落表**：没有新表、没有写 `usage` 表、没有新迁移。账本 = **事件重放**：hub 启动时 `Budgets::load_with_prices` 重放 `events` 表重建（`crates/hub/src/task_cost.rs` 的 `TaskLedger`）。每条 `agent.usage` 归到该 agent 最近被派的任务（worker 用 `task.status_changed` 的 assignee，评审人用新事件 `task.reviewer`）；换人、换会话、返工都累计、不重置，会话重启只清会话内累计。检查耗时计入（新事件 `task.check_timed`，以及 `workspace_run` 的 `task.command` 的 `duration_ms`）。新事件共三个：`task.budget`、`task.reviewer`、`task.check_timed`。
- **判定**（hub 的 `Budgets::check` 每 tick 一次）：`max_usd` 按价目表估算（含写缓存），仅当该任务所有有用量的模型都有价格才判；`max_tokens` 用 `tokens.total` 原始累计；`max_requests` 只有 agent 报了请求数且没有缺失才判；`max_minutes` 从首次 Running 到结束（或现在）；`max_idle_minutes` 仅 Running / Retrying / Verifying / Reviewing 状态、距上次进展；`max_rework` 在 `return_work` 闸门判断。
- **超限**：这个任务不再自动退回或续跑，交给 lead（`Decide`），写清哪项超了、用了多少，并注明「用量在回合结束时上报，实际可能已超出」，**不说成硬上限**。每个 (任务, 项) 只报一次，第一个超限项写进 `report.budget_over`。
- **价格或请求次数未知**：对应项不生效，lead 收到一次 `Silent` 提示（写明哪些模型缺价 / 请求次数未报）；估算金额为空、不显示 `$0`，`requests` 为空。
- **查看**：`usage_get` 的 run 行带 `tasks`；`crossagents usage --by task` 一行一个任务，列 `TASK / RUN / TOKENS / NEW IN / CACHE IN / CACHE WRITE / OUTPUT / REPORTED / ESTIMATED / REQUESTS / REWORK / MINUTES / CHECK SEC`，下面打印各行 notes。

## 6. Windows 消息直传与工具命名空间（含实测结果）

**背景**：Windows 上 `agent.cmd` 经 PowerShell 跑 `cursor-agent.ps1`，由它选最新的 `versions\<版本>\` 再用里面的 `node.exe` + `index.js`。因为走 `.cmd`，带换行的消息现在都写进文件，agent 要先 Read 一次（多一次模型请求）。

**做法**（#cursor-driver，`crates/drivers/src/argv.rs`，合并 `585d81d`）：

- 能解析出 `node.exe` 和 `index.js` 时**直接启动它们**，提示词照旧放在 `--` 后作为参数，换行原样传；解析不出来退回现在的 `.cmd` 加写文件；超过 `PROMPT_ARG_LIMIT`（24000 个 UTF-16 单元）的长消息仍写文件。**不走 stdin**（CLI 不支持从标准输入读提示词）。版本选法照 `cursor-agent.ps1`：脚本旁边有 `node.exe` 就用那一对，否则在 `versions\` 里取匹配 `YYYY.M.D[-HH-MM-SS]-<hex>` 的目录、按 `YYYYMMDD` 降序。
- 写文件的决定集中在 `argv.rs::prompt_via_file`，供 #rounds 发 `agent.prompt_via_file` 事件。
- **With 不是同类 shim**：`with.toml` 的 `batch-exe` 是把「用 `%*` 启动 exe」的 `.cmd` 展开成 exe，不是 `cursor-agent.ps1` 那种按版本目录选 `node.exe` + `index.js`。解析函数对这种命令返回 `None`，With 仍只在遇到换行、以及超长提示词时写文件。
- **命名空间**：原来插件目录是 `%TEMP%\crossagents-cursor-<随机 uuid>`，Cursor 给的命名空间 `plugin-crossagents-cursor-<uuid>-crossagents` 每开一个会话就变，agent 得重新查工具。实测确认 CLI 用 `plugin-${displayName}-${mcpServers 的键}` 拼，**目录名、manifest 的 name、完整路径都不进命名空间**，没有 `displayName` 时才用目录名。改法：每个会话仍有自己的目录（`mcp.json` 是这个会话自己的可写配置，不能多个会话共用一个可写目录），`plugin.json` 写死 `displayName = "crossagents"`；服务键是 hub 写死的 `crossagents`（`crates/hub/src/collab.rs`），命名空间固定为 `plugin-crossagents-crossagents`。

**实测**（#cursor-driver 交付，Cursor CLI `2026.11.05-86047df`，模型 `composer-2.5`）：

| 项 | 命令 / 输入 | 结果 |
|---|---|---|
| 命名空间 | `node.exe index.js -p --output-format=stream-json --model=composer-2.5 --force --workspace=<temp>\ns-probe\workspace --plugin-dir=<temp>\ns-probe\ns-parent-unique\ns-dirleaf --approve-mcps -- "Call the MCP tool named nsprobe exactly once. Then reply with only the full tool name you were given, nothing else."`；目录叶名 `ns-dirleaf`、父目录 `ns-parent-unique`，`plugin.json` `name=ns-manifest`、`displayName=ns-display`、mcpServers 键 `ns-server` | 工具返回 `NS_OK`；stream-json 里 server 是 `plugin-ns-display-ns-server`、工具名是 `plugin-ns-display-ns-server-nsprobe`；`exit 0`、`duration_ms 85090`。目录名、manifest name、完整路径都没进命名空间 |
| 多行直传 | 一条提示词 `"Reply with only the token on the next line.\nPAPAYA-42"`（不经 `.cmd`、不写文件） | 标准输出只有 `PAPAYA-42`，`exit 0`、stderr 空 |
| 参数原样到达 | rustc 1.99.1 的 `std::process::Command`（与 `crates/drivers/src/launch.rs` 同一套 Windows 转义）启动该 `node.exe`，`print-argv.js` 把 `process.argv` 用 hex 打出来比对 | `failed 0`；中文、😀、内部双引号、引号前的反斜杠、末尾反斜杠、`%OS%`（仍是 `%OS%`，不展开）、`^&|<>`、`\r\n`、单独换行、以及把这些拼成一条提示词都没丢字符 |
| 长度 | `PROMPT_ARG_LIMIT = 24000`，比较是 `text.encode_utf16().count()`（`argv.rs:161`），与 Windows 命令行 32767 个 UTF-16 单元同一单位 | 实测参数前缀约 639 单元，余量约 8128；24000 个「中」、24000 单元的 emoji、24000 单元里夹 2822 个引号和反斜杠的中文都原样到达 |

评审登记的非阻塞问题（P2/P3）及遗留见 §11.1；其中「`versions` 下有 `*.new` 时先走脚本让它升级」「提示词长度按引号转义后的 UTF-16 长度算」两条已由 #rounds（`07875a2`）顺手修掉，**不再是遗留**，这里只记一句。`cargo test -p crossagents-drivers`：200 passed、2 ignored。Cursor CLI 的协议事实见 [cursor-cli 记录](../research/cursor-cli.md)。

## 7. 每轮新会话与结构化交接

**现状**：`run_next` 让 lead 在原来的会话里接着做（Cursor 用 `--resume=<chat>`），上下文越背越大。

**做法**（#lead-session，合并 `f95fe4b`）：

- `run_next` 开新一轮时，hub 给 lead 开**新的模型会话**：Cursor 走 `create-chat`，其他驱动用各自开新会话的方式，不支持的保持现状并说明。仍是同一个 agent（id、名字、GUI 里的对话入口不变）；旧会话的 id 照旧记在事件里能查到；两轮之间人和 lead 的对话留在旧会话。**只在 `run_next` 换会话**，`scheduled`（要等回合结束才移动会话）与恢复已归档会话的路径照常工作。
- 开轮消息（`crates/orchestrator/src/brief.rs` 的 kickoff）带**结构化交接，从 hub 的状态生成，不靠模型摘要**：原始目标（这一轮请求原文）、有效约束（team 笔记 `constraints` 原样放进）、上一轮结果（summary 和落地情况）、未决事项（没处理完的 issue、等人的事项、没落地的改动；Completed 与 Failed 都写未落地改动那一行）。开轮消息里提醒 lead：长期有效的约束写进那条 team 笔记。
- 续会话的可靠性（第 4 版返工）：`Inbox::close_if_idle` 与 `pop_turn` 在同一把锁下判定，忙时不关、队列不丢；「空闲才关」不再轮询，`renew` 在总线上等 lead 回合结束；排队消息带进新会话（carried），停止时撤回并发布 `AgentInputWithdrawn`；`renew` 不再 `forget` 客户端；打开新会话不再报丢上下文警告。lead 一直忙、队列一直有消息时，换会话会拖到 `round_wait_limit`（默认 120 秒），之后这一轮就在旧会话里开（日志里有警告）。

遗留见 §11.5；验收：hub 198 单元测试 + orchestrator 252 + `run_next` 17 / `resume_command` 4 / `session_reuse` 13 / `delivery` 9 / `delivery_state` 4 全过。

## 8. 旧日志回放：估算能少叫醒多少次

脚本 `scripts/replay-wakeups.mjs`（合并提交 `36c63dd`），Node 直接跑，不用编译。命令（仓库根目录）：

```
node scripts/replay-wakeups.mjs --agent 01a1b6b3-5a01-7229-85c6-a0c986bf5627 C:/Users/Link/.crossagents/logs/events-2026-11-07.jsonl C:/Users/Link/.crossagents/logs/events-2026-11-08.jsonl
```

加 `--to 2026-11-08T17:29:51Z` 就截到上一轮结束，不含这一轮自己的回合。数据是主导 agent（Zara 4）11-07、11-08 两天的日志；**结果全部是估算**；主导 Zara 4 在接受前自己重跑核对过。

| 范围 | 实际唤醒 | 实际输入 | 乐观 | 保守 | 参考 |
|---|---|---|---|---|---|
| 两天全部 | 165 次 | 197.3M | 96 次（**-42%**） | 109 次（-34%） | 119 次（-28%） |
| 截到 11-08 17:29:51Z | 132 次 | 162.8M | 78 次（**-41%**） | 89 次（-33%） | 99 次（-25%） |

三列的意思：**乐观** = 「等接受」全按 `Batch`；**保守** = 全按 `Unblock`；**参考** = 交付报告仍像现在的代码一样立刻叫醒。全按 batch 会高估节省，不是偏保守。

- **乐观的节省构成**（两天全部，共 69 次）：`Silent` 不再单独叫醒 45 次、`Batch` 合并 23 次、`Decide` 并进已有唤醒 1 次。
- **没做决定的回合**（两天全部）：按事件算 81/165（49%）；按工具算 122/162（75%）；合并口径 125/165（76%），占输入 105.4M（53%）。事件口径偏低，原因见脚本输出：工具记录不全（只有 hub 工具带名字）、`passed→running` 多半是 hub 自动返工、`merged`/`cancelled` 没有 actor、时间重叠就算到 lead 头上。

**7 条假设**（照抄脚本输出要点，改动任何一条数字就会变）：

1. 交付报告 / 评审通过算「等接受」：旧日志里它们是 worker 的消息，现在的代码会立刻叫醒（`crates/hub/src/notices.rs:224` 的 watch 行也是 `Decide`）；本估算按新流程（#review-loop，检查和评审都过了、hub 发「等接受」）当 `Batch`。按现状算见「参考」。
2. watch 的 passed/merged 行按内容归类（passed → 等接受，merged → silent），不按 watch 一律 decide；否则「等接受」永远合并不了。
3. 依赖图只能部分重建：`run.plan_submitted` 只有任务 key；`agent.tool_detail` 的 `task_get`/`task_list` 结果里有 `depends_on`，但只覆盖少数任务（评审数到 8 个任务、3 个有依赖），重建不全。所以给区间（batch 乐观 / unblock 保守），**不用部分依赖算中间数**。
4. 新叫醒不占时间：只避开日志里实际存在的回合，新规则产生的回合自身多长没有模拟；叫醒消息发出后、回合开始前到达的通知不会并进去。
5. 消息不合并：每条消息各自一次唤醒（忙时顺延到前一个回合结束）。待带的通知只跟人的消息走：已合并的代码只在 `from == MessageSource::Human` 时吸收待带通知并撤掉排队中的叫醒消息（`crates/hub/src/runtime.rs:1940-1952`），`brief/common` 也只写「人的消息…带上所有待带的通知」；队友和协调器的消息不带，通知留在 pending，`Batch` 窗口也不重置（「参考」里的交付报告也不带）。
6. 通知到达时间按摘要文本首次出现推断；被撤回的摘要里的行没有计入。
7. `Batch` 边界：最后一条之后正好第 60 秒（或第一条起第 180 秒）到达的通知，先叫醒，它算下一批。

读法：下界 = 「等接受」全按 batch（乐观，叫醒更少、节省更多），上界 = 全按 unblock（保守）；旧日志没有完整依赖图，给不出中间数。旧日志里没有 `agent.round` / `notice.recorded` 时，脚本按通知原文和消息来源推断（输出里标「推断」）；有这些事件时按原值用。**没有模拟**：新规则产生的回合自身多长（假设 4）。

## 9. 按阶段换档：实测结论与下一轮

#phase-model **只交报告、没改代码**。测法：Cursor CLI（`C:/Users/Link/AppData/Local/cursor-agent/cursor-agent.cmd`，不在 PATH 上），`-p --output-format=stream-json --stream-partial-output --resume=<chat> --model=<M> --trust --sandbox=disabled --workspace=<临时目录> -- "Say OK."`。`--sandbox=enabled` 在 Windows 上直接报错退出（`Sandbox requires macOS or Linux`）。

思考档位直接编在型号名里（`claude-opus-5-5-low/-medium/-high/-xhigh/-max`，各有 `-fast`；没有裸的 `claude-opus-5-5`）。**换档 = 换 `--model`，不用重开会话**；`cursor_native` 的 `set_model`（`crates/drivers/src/cursor_native/mod.rs:457`）只存型号，下一回合生效。

**Opus 5.5（同一个 chat 里换档）**：

| 回合 | 档位 | input | cacheRead | cacheWrite | output |
|---|---|---|---|---|---|
| 1 | medium | 4 | 9241 | 11351 | 5 |
| 2 | 换到 max | 4 | 20592 | 65 | 49（**整段前缀都命中**） |
| 3 | max | — | 20657 | 111 | — |
| 4 | 换回 medium | — | — | — | — |

第 4 回合换回 medium 时网络错误（`Client network socket disconnected before secure TLS connection was established`），**没进模型、没重跑（要实测）**，所以没有用量数字。新开一个 chat（同一工作目录）用 max：第一回合就读到 20535、只写 57 → **系统提示词这段固定前缀跨会话共用**。

对照 `gpt-5.3-codex`：**缓存按档位分开存**，换到本会话没用过的档位时整段未命中（low 12214/0 → high 12261/0 → high 148/12160 → low 323/12032，前一个数是未命中输入、后一个是 cacheRead）；codex 下 CLI 的 `cacheWriteTokens` 一直是 0，Claude 下不是 0。两组都没有 `cost_usd`（CLI 不报）。共 **9 个真实回合**跑在 Cursor 账号上，都没碰到限额错误——hub 判定「Cursor 账号已到限额」并把任务改派给 Claude Haiku，这个判定很可能不对（待评估，见 §11.9）。

**读法**：在 Opus 上，换档的缓存代价几乎为零；第一版报告里「每次切换付整段未命中」的理由**只对 codex 成立**。但测的对话只有几十个 token；真实 lead 的对话历史有上百万 token，换档后这部分还命不命中没测到。值不值得做，还取决于协调阶段降档能省多少输出、质量会不会掉，只能在真实 lead 回合上量。

**决定：这一轮不写代码，留到下一轮**。方案（worker 建议）：`RunPolicy` 加可选的 `lead_models { plan, coordinate }`（`crates/core/src/policy.rs`、`bindings/RunPolicy.ts`），不设就不换档；hub 只在阶段切换时调 `set_model`（开轮和整轮验收用 `plan`，计划批准后用 `coordinate`），人的消息不触发切换；测试覆盖切换时机。与 #task-budget 的 `RunPolicy.task_budget` 字段不冲突（谁后合并谁 `branch_sync`，两边字段都留）。

**下一轮先测（要实测）**：换回低档那一步的缓存；真实 lead 会话中途换档后长对话的缓存命中；缓存能留多久；协调阶段用低档的输出占比和质量。

## 10. 落地与生效、整轮验收

- 本轮九个任务的代码已合并（`d0fe82d`），**新功能已随 0.2.12 生效**（2026-11-09 发版 `cdfe86b`，本机 hub 已在跑 0.2.12）。历史：这一轮自己是在 **hub 0.2.11** 上跑的，仍按 0.2.11 的流程：每条通知立刻叫醒、lead 手动开评审、手动接受，没有任务预算和结构化交接。人当时在 Mac 上测远程 GUI，连的就是这台机器上正在跑的 0.2.11；本轮没有停、重启或升级它，没有改 `~/.crossagents` 下的文件（只读可以）。测试要起 hub 用临时 home 和端口，沿用现有测试的做法。
- 约束（`brief/common`）：只跑这次改动需要的测试，全量检查由 hub 在落地前跑；花钱的真实 agent 回合用最便宜的模型、最短的提示词，把命令和结果写进交付；改了带 ts 导出的类型就重新生成 bindings、GUI 的类型检查要过；这台机器的 python 是 Windows 商店占位符，不能用。
- **整轮验收**（`brief/common`）：重启不丢待办；重复消息不会重复返工；旧评审不能用来接受新代码；等接受不会卡住下游。
- 组合验证：#review-loop 与 #lead-session 各自在自己的分支上验收，先后合并后的组合没有人自动验证；主导在 run 分支 `5b1a6d1` 上手动跑了 `crossagents-orchestrator` 和两边相关的 8 个 mock 测试文件，全过（见 §11.9）。

## 11. 遗留

> 本节是 `lead/leftovers`（2026-11-09 03:03 更新版）的完整落点，逐条对应；同一批条目在 [TODO.md §二十八](../TODO.md) 是勾选清单。标记：**【实测】** 还要在真实 agent / 真机上量；**【下一轮】** 实现或改动留到下一轮；**【待评估 / 待定】** 判定或做法未定；**【已知】** 已知行为，本轮不改。已修、不再是遗留的两条在这里写明一句。

### 11.1 Cursor 驱动（#cursor-driver，已合并 `585d81d`）

- 【下一轮 / 低优先】同一天的版本目录用目录名降序打平（`crates/drivers/src/argv.rs:52`）。`YYYY.MM.DD-<hex>` 和 `YYYY.MM.DD-HH-MM-SS-<hex>` 混在同一天时，更晚的构建可能排到后面；`cursor-agent.ps1` 自己对同一天也不固定顺序。本机的两个版本不是同一天，不影响现在的选择。
- 【下一轮 / 低优先】插件 `displayName` 写死 `crossagents`，命名空间固定为 `plugin-crossagents-crossagents`（`crates/drivers/src/cursor_native/mod.rs:496`）。同一个 CLI 进程若再加载 `displayName` 和服务键都叫 `crossagents` 的插件会撞名；本机已装的 stripe、outlook、gmail、x 不撞。
- **已修、非遗留**（#rounds `07875a2`）：`versions` 下有下载好、待升级的 `*.new` 版本时改走脚本让它先升级、下次再直接启 `node`；提示词长度按引号转义后的 UTF-16 长度算。这两条不再是遗留，见 §6。

### 11.2 通知（#notices，已合并 `ee04aad`）

- 【已知】已归档 agent 的待带通知不投递、也不清理，行留在 agent id 上（故意如此：同一个 agent 恢复后还能看到）。删除 agent 时才清。

### 11.3 回合记录（#rounds，已合并 `07875a2`）

- 【下一轮】hub 重启前已在排队的消息，重启后跑出的回合没有 `sources` 和 `notices`；`recover` 为被打断的回合补发的 `TurnCompleted` 也会产出一个没有来源的回合。原因：`rounds` 只看本次 hub 发出的事件，`requeue` 不重发输入事件。要修得在启动时把未完成消息的事件灌回 `rounds`，或 `requeue` 时补发输入事件。
- 【已知 / 原有缺口】stream rule 排队注入（`crates/hub/src/runtime.rs` 的 `inject` 直接 `inbox.push`，不写 `agent.input`）跑出的回合没有来源。stream rule 在回合中插话的那条路径记为通知（`Notice`）。
- 【下一轮】叫醒消息的 `notice.carried` 在投递成功后才发；如果那一回合在它发出前就结束了（比如投递后立刻失败），这一回合缺这些通知，事件随后丢弃。外部 agent 取消息很快时也可能碰上。
- 【已知】外部 agent（`take_external`）的回合错后一条消息：取走消息时就补发开始和结束，之后调的工具落到下一个回合；用量不报（`null`）。
- 【下一轮】`usage --by round --run` 只认编排器发出的消息；人发给该 run 成员的消息开始的回合不算进去。
- 【实测】Cursor / With 驱动发 `PromptViaFile` 没有单元测试；没在真实 agent 上跑过 `usage --by round` 的端到端输出。
- 【实测】Codex 恢复会话的用量修正（从本会话第一次调用算起，起点 = 第一条通知的 `total − last`）只在单元测试里验证，没在真实 Codex 会话上跑过。如果 Codex 在恢复线程时先重发一条旧的用量通知，上一次调用会被多算一次。恢复或分叉出来的 Codex 会话，请求次数仍记为未知（`null`）。

### 11.4 评审闭环（#review-loop，已合并 `f94bee7`）

- 【实测】同一个评审 agent 跨轮（`reviewer_fresh=false`）在 mock 里跑不起来（驱动不能在另一个目录恢复会话，计划被拒），只有单元测试覆盖。
- 【实测】hub 重启后恢复 `Reviewing` / `Passed` 的任务只有单元测试（`crates/orchestrator/src/tests_verification.rs`），没有 mock 重启测试。
- 【实测】原 worker 会话已不在、退回时换新会话的路径没有 mock 测试。
- **已修、非遗留**（#task-budget `2201c36`）：`review_submit` 比对 `reviewed_sha` 原来要求完整相等、拒绝信息却给 8 位短提交号；现在去空白、不分大小写、至少 7 位的前缀算同一个提交，拒绝信息给完整提交号（见 §4）。
- 【生效情况，非遗留】新的评审闭环已随 0.2.12 生效（本机 hub 已在跑 0.2.12）；这一轮自己还是按 0.2.11 的流程走的（lead 手动开评审、手动接受）——见本文开头与 §10。

### 11.5 每轮新会话（#lead-session，已合并 `f95fe4b`）

- 【下一轮 / 单开任务】已归档的 lead 通过 `/v1/works/{id}/runs` 开新一轮：轮次建起来了、lead 的会话也重启了，但开轮消息送不到，run 一直不结束。#lead-session 的 worker 在改动前的 `a38a1cd` 上就能复现；评审确认这条路径 `create_work_round` 传 `source=None`（`crates/orchestrator/src/work.rs:275`），不走本轮的续会话和交接，`send_envelope` 对已归档的 agent 返回 Conflict。原有问题，要单开任务修。
- 【已知】排队中的消息只在内存里：续会话后、新会话打开前 hub 重启，这些消息的去向和以前一样（本轮没有新增保护）。
- 【实测】「空闲才关」和旧会话取下一条消息之间的竞争只有 Inbox 的单元测试，没有端到端测试。
- 【已知】lead 一直忙、队列一直有消息时，换会话会拖到 `round_wait_limit`（默认 120 秒），之后这一轮就在旧会话里开（日志里有警告）。

### 11.6 任务预算（#task-budget，已合并 `2201c36`）

- 【已知】预算都没配时整套检查跳过，**默认不起作用**；下面几条只在配了预算时会碰上。
- 【下一轮】`max_idle_minutes`（最长无进展时间）只认任务状态、进度、检查、命令和用量事件（`crates/hub/src/task_cost.rs:126-234`），不认 agent 在回合里的工具调用和输出。用量到回合结束才报，一个 25 分钟的长回合会被算成无进展：配了这项就可能误报超限，并停掉这个任务的自动退回。要把被派 agent 的回合活动也算作进展。
- 【下一轮】故障自动重试（`crates/orchestrator/src/attempts.rs` 的 `retry_same` / `retry_new_session`）不看 `report.budget_over`：超限的任务出错时仍按 `max_attempts` 自动重试。`attempts.rs` 不在本轮范围。
- 【实测】hub 的预算检查（`crates/hub/src/budget.rs` 的 `check_tasks`，按 tick 跑）没有端到端测试，只测了纯函数和编排器发的通知；`crates/mock-agent/tests/task_budget.rs` 没写；`crossagents usage --by task` 没在真实数据上跑过，没有输出样例。
- 【待评估】任务账本不落表：hub 启动时重放 `events` 表重建（`Store` trait 在 `crates/core/src/store.rs`，不在本轮范围）。重启不丢，但事件越多启动越慢；`usage` 表仍然没人写。
- 【已知】范围外的改动：`crates/cli/src/cmd/events.rs` 给三个新事件（`task.budget`、`task.reviewer`、`task.check_timed`）加了 match 分支；`bindings/TaskReport.ts` 重新生成；`TaskSpec` 和 `PlannedTask` 去掉了 `Eq`（`TaskBudget` 含 `f64`）。

### 11.7 换档（#phase-model，只交报告）

- 【下一轮】实测数据、方案和下一轮要先测的几条见 §9 与 run 笔记 `lead/phase-model`；**实现留到下一轮**。下一轮先测（【实测】）：换回低档那一步的缓存；真实 lead 会话中途换档后长对话的缓存命中；缓存能留多久；协调阶段用低档的输出占比和质量。

### 11.8 测试（偶发失败，本轮未改测试与辅助函数）

- 【已知】`crates/mock-agent/tests/continuation.rs` 的 `a_task_that_moves_within_the_threshold_is_not_reported` 偶发失败（期望 0 次续跑叫醒，实际 1 次）。#notices 的 `ee04aad` 第一次验收挂在这条，代码不动用 `task_verify` 重跑四条全过。原因：hub 的后台时钟可能插在 `quiet()` 和更新任务之间跑一次续跑检查，这条测试没像 `quiet_until_woken` 那样防。后面任务的验收再碰上就 `task_verify` 重跑。
- 【待评估】`crates/mock-agent/tests/run_policy.rs` 的 `the_stuck_report_is_set_kept_and_turned_off` 偶发失败过一次（`run_policy.rs:162`，设 `stuck_minutes=15` 后回复里是 None）：#lead-session 第四版第一次验收挂在这条，主导单独重跑这个文件 8 条全过，`task_verify` 整组重跑也全过。这次改动没碰策略路径，疑是改策略和其他写 run 的路径并发，原因没查。另外 cargo 在第一个失败的测试文件就停，后面的文件不跑：验收命令可以考虑加 `--no-fail-fast`。
- 【已知】`crossagents-core` 的 `exec_tree` 测试 `exec_waits_for_a_live_predecessor_root_process`（断言 `elapsed >= 1s`）全量跑时偶发失败，单独重跑通过（#rounds 的 worker 报告）。
- 【已知 / 疑似时序】`crates/mock-agent/tests/stream_rules.rs` 的 `a_rule_goes_once_in_twenty_turns`、`compaction_lets_the_rule_go_again`、`the_agents_own_compaction_lets_the_rule_go_again` 在整组跑时失败过一次，单独重跑通过（#lead-session 的 worker 报告）。

### 11.9 hub 平台

- 【待定，问过人】任务验收每条命令的超时写死 600 秒（`crates/orchestrator/src/verify.rs:57`），`crossagents.toml` 的 `timeout_secs` 只管落地前的检查。这一轮靠把验收拆成多条命令绕过。是否做成可配：待定。
- 【下一轮】`task_verify` 只能整组重跑验收，不能只重跑挂掉的那一条；一条偶发失败就要再等整组（约 20 分钟）。
- 【下一轮】空的 `scope.allow` 被当成「可能改任何文件」（`crates/orchestrator/src/plan.rs:147-150`），计划里不写范围就全部串行。协议文本或 `plan_submit` 应该提醒 lead 写范围。
- 【下一轮】验收通过后 worker 又交一次会让检查重跑（#notices 发生了两次，每次约 12 分钟）。
- 【下一轮】验收有一条没过时，任务状态仍显示 `passed`（只表示验收跑完了），靠 `task_accept` 拒绝来拦；任务列表里看不出没过。
- 【待评估】`usage_get` 显示 Cursor 账号 `limit_reached=true`，但月度窗口只用了 22%，而且 #phase-model 在 Cursor 账号上跑了 **9 个真实回合都没碰到限额错误**：这个判定很可能不对，依据待评估。因为它，#rounds、#review-loop、#lead-session、#task-budget、#phase-model 都被改派给 Cleo 2（Claude Haiku）。
- 【已知】任务的 mock-agent 测试不在验收里时（如 #rounds），改了共用类型的编译问题要等下一个跑 mock 的任务才暴露；主导靠人工查构造处兜底。
- 【已知】两个任务各自在自己的分支上验收，先后合并后的组合没有人自动验证：#review-loop 和 #lead-session 合并后，主导在 run 分支 `5b1a6d1` 上手动跑了 `crossagents-orchestrator` 和两边相关的 8 个 mock 测试文件，全过。
- 【下一轮】Claude Haiku 的会话多次以 `end_turn` 收尾却没交结果：#lead-session 的 worker 没汇报、任务 stalled；#replay 的评审把意见写在回复里、没提交结论；#task-budget 的 worker 在后台自己跑整组测试、**等 Monitor 通知时结束了回合**（回合结束后这种通知叫不醒它）。hub 只发一条「没汇报 / 没结论」的通知，要 lead 介入。可以考虑：回合以 `end_turn` 结束、任务还在 running 或 reviewing 时，hub 自动提醒一次「用工具交结果」；评审把意见写在回复里时，把回复文字附在通知里；worker 的协议文字写明「不要等后台任务的通知」。经验库已记：`dc8dc64a`（返工说明里写明「这个回合做到交付，报进度后不要结束回合」之后，同一个 worker 一个回合就交付了）。
- 【下一轮】lead 用 `agent_send` 的 `now:true` 打断 worker、插一条消息时，被取消的回合仍按「没汇报就结束」处理：任务标 `stalled`、叫醒 lead，其实那条消息已经在排队、新回合马上就开（#docs 这次：回合取消到标 stalled 隔 6 毫秒，0.2 秒后新回合开始）。worker 一报进度或交付就恢复（`crates/orchestrator/src/tasks.rs:607`、`:707`），只是白叫醒 lead 一次。改法：回合以 `cancelled` 结束、而这个 agent 还有排队消息时，不标 stalled、不发通知（`crates/orchestrator/src/attempts.rs:283-286` 的 `turn_ended`）。

### 11.10 其他

- 【下一轮】`scripts/release.mjs:42` 的注释过时：`build.extraResources` 应为 `build.win.extraResources`。
