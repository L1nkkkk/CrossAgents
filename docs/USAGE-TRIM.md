# CrossAgents：减少额度消耗（第二轮）

> 日期：2026-10-31 · 基线：0.1.20 · 状态：阶段 1、2 已实现，阶段 3 暂缓。  
> 前一轮见 [SESSION-FIRST.md](SESSION-FIRST.md)（独立 lead、普通聊天精简）。

## 1. 原则

只减少**重复发送的信息**和**只为确认一句话而启动的模型轮次**。不砍协作、lesson 或其他现有功能，不换便宜模型，不自动压缩历史。

能控制的只有 hub 自己注入的说明、工具定义、工具结果和消息。原生客户端读文件、跑终端产生的上下文、它自己的系统提示和压缩，不在这里处理。省下多少以实测为准，不预设比例，也不从 token 推算订阅额度。

## 2. 顺序

| 阶段 | 内容 | 状态 |
|---|---|---|
| 1 | Work 提案的开始、拒绝、关闭通知静默化 | 已实现 |
| 2 | `lesson_get` 证据分页；随后精简 `lesson_search` 返回 | 已实现 |
| 3 | hub 工具的长结果按需展开 | 暂缓，先看实际用量 |

不做：缩短常驻工具说明、给队友列表加分页、另造通知合并机制（§6）。

## 3. 阶段 1：Work 提案通知静默化

### 3.1 现在的问题

`work_decided`（[work.rs](../crates/orchestrator/src/work.rs)）在三种情况下用 `deliver` 给提案的聊天发消息，每次都启动一轮父模型：

- 点「开始」：告诉它 run 已开始、谁主导、在哪个分支。父模型多半只回一句「已经开始了」。
- 点「不用」或输入别的回答：告诉它 “do it here in the conversation, or keep talking”。
- 关闭卡片（`dismiss_human`）：同上。

这些轮次带着整段对话历史，只换来一句确认；拒绝时还可能被模型理解成「马上在主工作区做」。

### 3.2 新行为

三种情况都改用已有的 `tell_later`（与 run 结束摘要同一机制）：记录时不启动会话、不打断当前轮次；父聊天下一次收到消息时带上，消费一次。提醒和 watch 的主动唤醒不变；已有 digest 合并的是会唤醒模型的通知，这里不动它。

**开始**，主题 `run:<run_id>`：

> CrossAgents work started: "<标题>" (run <id>), led by <lead 名字> on branch <分支>, from your brief. This conversation stays as it is; the Work goes on there.

与结束摘要共用 `run:<run_id>`：如果用户一直没回父聊天，结束结果覆盖「已开始」，下次只看到最新状态。

**拒绝或关闭**，主题 `work:<卡片 id>`，措辞只陈述事实、不给执行指令：

> The person did not accept your proposed Work "<标题>", so no run was created. What to do next is up to the person's next message.

拒绝开分支不等于取消原始需求，也不等于授权在主工作区马上执行。

### 3.3 开始与结束的先后

run 的结束摘要在终态保存之后写入（`set_status` / 落地）。启动通知在 `create_run` 返回之后写入，此时 lead 已经收到 kickoff 在工作；极快结束的 run 可能先写了结束摘要，再被启动通知覆盖。顺序固定为：

1. 写入启动通知；
2. 重新读取 run；
3. 若已是终态，再写一次结束摘要（`tell_origin_later`）。

结束摘要总在终态保存之后写，所以无论 run 在第 1 步之前还是之后结束，最后留下的都是结束结果。最坏情况是父聊天在两步之间刚好消费了结束摘要，随后又收到一遍同样的结束摘要，不会留下过时的「已开始」。

### 3.4 界面

原来父聊天里那条「The person started the Work…」消息就是开始或拒绝的唯一记录；静默后要由界面补上：

- 聊天时间线里，按卡片的决定时间显示一行记录，和模型切换、交接同一类：
  - 开始：「开始了 Work「标题」」，点它进入 run；
  - 拒绝或关闭：「没有开始 Work「标题」」。
- 数据来自已有的提案卡片（`action: work`、`from` 为这个聊天、`answered_at`），不新增事件或接口。
- 关闭卡片现在不记 `answered_at`，补上，记录才有时间（缺失时退回 `created_at`）。

侧边栏的嵌套和点「开始」后跳到 run 页面保持不变。

### 3.5 顺带清理

`work_decided` 里「提案者自己主导」的分支在 0.1.20 已经走不到（总是独立 lead），删掉。

### 3.6 验收

| 场景 | 结果 |
|---|---|
| 点「开始」 | 父聊天不启动新轮次；独立 lead 照常立即工作；父聊天下一条消息带上启动通知 |
| 点「不用」、关闭卡片 | 父聊天不启动新轮次；下一条消息带上新措辞，不含 “do it here” |
| run 在启动通知之前结束 | 留下的是结束摘要（确定性单元测试：先让 run 进入终态，再写启动通知） |
| run 在启动通知之后结束 | 结束摘要覆盖启动通知 |
| 提醒、watch | 照常主动唤醒 |
| 聊天时间线 | 开始 / 没有开始各显示一行，开始的可点进 run |

`tell_later` 失败（宿主不支持）只记警告，不退回 `deliver`：退回就会重新唤醒模型，界面上的记录照常显示。

测试：`work_start.rs` 的 `starting_and_ending_a_child_never_wake_the_parent`、`the_parents_next_message_says_its_work_started`、`declining_or_closing_a_proposal_does_not_wake_the_parent`；先后顺序由 `tests_verification.rs` 的两个单元测试确定性覆盖（先让 run 进入终态再写启动通知、结束摘要覆盖启动通知）；时间线记录由 `workNotes.test.ts` 覆盖。提醒与 watch 的唤醒由原有 `collab.rs` 测试覆盖。

### 3.7 需要同步的文档

- [SESSION-FIRST.md](SESSION-FIRST.md) §2、§6.2：「开始/不用通知可能产生一次父模型确认回复，v1 保留」改为已静默。

## 4. 阶段 2：lesson 返回量

lesson 机制保留。常驻部分只有一行提示（[lessons.rs:101](../crates/orchestrator/src/lessons.rs:101)），不随记录数增长；增长点在读取结果。

### 4.1 `lesson_get` 证据分页（先做）

现在 `lesson_get` 返回整条 `Lesson`，包括全部 `evidence`（[lessons.rs:422](../crates/orchestrator/src/lessons.rs:422)），每次 `lesson_report` 都追加一条，用得越多越长。

默认返回：

- 完整的 `title`、`symptom`、`conditions`、`resolution`、`outcome`、`status`、`superseded_by`、`keywords`；
- 证据统计：总数、按 outcome 计数、最早和最近时间；
- 最近 5 条证据，新的在前；
- **无论多早，最近一次失败（`unresolved`）的证据都单独给出**；`status` 为 `review` 时排在最前说明原因。重要的失败信息不能藏在分页里；
- `sources` 只给数量和最近几条引用。

参数 `evidence_offset`、`evidence_limit`（上限 50）分页读取其余证据。所有证据照常保存，`lesson_report`、整理工具和 GUI 读取的 REST 接口不变。

### 4.2 `lesson_search` 精简（随后）

现在默认 5 条，每条带完整 `symptom` 和 `resolution`（[lessons.rs:398](../crates/orchestrator/src/lessons.rs:398)）。改为每条给标题、状态、适用条件摘要、症状和方案的前一段，截断时标明，需要全文用 `lesson_get`。搜索负责筛选，详情负责确认适用性。

### 4.3 需要同步的文档

- [tasks/LESSONS.md](tasks/LESSONS.md) §4.5：「`lesson_get { id }`：返回全文，包括 evidence」改为上面的默认内容和分页参数。

### 4.4 验收

| 场景 | 结果 |
|---|---|
| 证据很多的 lesson | 默认只带统计和最近 5 条；分页能读到全部 |
| 早期有失败、近期都成功 | 最近一次失败仍在默认返回里 |
| 状态为 review | 默认返回里看得到状态和导致复核的那条证据 |
| 搜索 | 条数不变；单条长度有上限，截断有标记 |

实现：证据按新到旧排，`evidence_offset` 从最新一条数起；状态说明放在顶层 `review` 字段（JSON 键序不固定，所以不靠排在前面）；搜索每个字段取第一段、至多 300 字节。测试：`lessons.rs` 的 `a_long_history_shows_counts_and_the_latest_tries`、`a_lesson_under_review_says_why_up_front`、`a_search_hit_cuts_long_text_and_says_so`；mock-agent `lessons.rs` 在报告失败后用 `lesson_get` 看到复核说明和那条失败。

## 5. 阶段 3：长结果按需展开（暂缓）

能管的只有 hub 工具：`workspace_diff`、`workspace_read`、`workspace_run`、`task_get`、`checks_run`。原生客户端的文件和终端输出不经过这里。检查命令输出已有 8,000 字节尾部上限（[verify.rs:58](../crates/orchestrator/src/verify.rs:58)）。

动手前先从用量记录里找出实际返回最大的 hub 工具，只改那一两个：保留退出码、关键错误、截断标记和读取完整结果的入口，不能把关键错误截掉。没有数据支持就不做。

## 6. 不做的事

- **缩短常驻工具说明。** 说明直接影响模型怎么用工具，为小幅节省冒行为退化的风险不值得。也不能假定它们每次都命中缓存。
- **`agent_list` 筛选分页。** 只在调用时付费，等实际变长再说。
- **另造通知合并。** digest 已合并模型忙碌期间的同主题通知；提醒和 watch 需要唤醒，不并入静默通道。
- **删掉普通聊天的 lesson 整理工具、自动摘要历史、强制换模型或减少协作。** 都会改变现有能力。
