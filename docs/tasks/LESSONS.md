# LESSONS 任务书：agent 之间共享的教训

> 依据：2026-10-28 讨论（让 CrossAgents 自我迭代，不一定改代码：执行中踩到的坑记下来，后面的 agent 都能看到）· 状态：设计，未开工 · 2026-10-28

## 1. 目标

一个 agent 踩过的坑，下一个 agent 不再踩。比如：
- 「这台机器上 `python` 是 Windows Store 的占位程序，用 node 写脚本」；
- 「pipeline.rs 是 CRLF，字符串替换前先统一换行」；
- 「`cargo fmt --all` 会改到无关文件，只格式化改过的包」。

这些今天只存在于某次会话的记录里，会话结束就没人记得。

要做到三件事：
- **随手记下**：agent 在做事的当口就能记，不必等到 run 收尾。
- **人能把关**：教训会被注入给所有 agent，写坏一条，所有 agent 都会被带偏。
- **自动送达**：新开的 agent 开场就能看到；正在跑的 agent 能收到新增的条目。

和自改代码相比，这是成本最低的自我迭代：不动代码、不过验收，改错了删掉一条就行。

## 2. 现状与缺口

| 已有 | 能做什么 | 缺什么 |
|---|---|---|
| 项目规则（[DOGFOOD.md](DOGFOOD.md) §6，`crates/hub/src/rules.rs`） | `CROSSAGENTS.md` → `AGENTS.md` → `CLAUDE.md`，开会话时注入给所有 agent | 人写的文件，agent 不该改：改坏了没人发现，而且会进 diff 和人写的约定混在一起 |
| `team` 笔记（[H1.md](H1.md) §5，`note_write` / `note_read`） | 跨 run 保留的共享笔记，工具说明里就写了可以记「踩过的坑」 | 不注入，要 agent 自己想到去读；不分仓库；没人审，写了就生效；没有长度上限，不适合塞进开场说明 |
| 复盘（[SELF.md](SELF.md) C1） | 读已结束 run 的 trace，写成「现象 / 影响 / 方向」文档 | 产出的是给人看的长文，不是给 agent 的一句话 |

教训是第四种东西：一两句话，经人确认，自动注入，按仓库区分。它与上面三者的关系：
- 规则是人定的约束，优先级最高；教训只是经验，与规则冲突时以规则为准。
- `team` 笔记留着放长文和约定的细节，教训里可以引用笔记的 key。
- 复盘的结论可以提成教训（§4.2）。

## 3. 为什么不直接让 agent 改 `CROSSAGENTS.md`

这是最直观的做法，不采用，有三个原因：

1. **写入时机不对。** agent 在 run 的 worktree 里改文件，改动要等 run 落地才进主分支。可教训最多来自失败的 run，而失败的 run 不落地，教训随之丢失。
2. **人写的规则会被改坏。** 规则文件是人的约束（DOGFOOD §6「the person keeps these」），agent 改它等于自己放宽自己的约束，和 [SELF.md](SELF.md)「不交给 agent 的」第一条冲突。
3. **一个 agent 写坏，所有 agent 都中招。** 规则会注入给每个 agent，一条写错的或者被注入了恶意内容的教训会扩散到所有会话，所以需要一道人的确认。

所以教训由 hub 保存、按状态管理，注入时排在规则后面，并单独成节。人想把一条教训升格成规则时，自己抄进 `CROSSAGENTS.md`。

## 4. 设计

### 4.1 一条教训

| 字段 | 说明 |
|---|---|
| `id` | |
| `scope` | `repo`：只对某个仓库的 agent（按主仓库路径归属，同 `workspace::repo_key` 的算法，worktree 归到它的主仓库）；`global`：对这台 hub 上的所有 agent（「claude.com 要走 7890 代理」这类和仓库无关的） |
| `text` | 给 agent 看的正文，最多 400 字节，一两句话：在什么情况下、该怎么做 |
| `why` | 怎么踩到的，给人审时看，不注入 |
| `source` | 出处：agent、run、task、消息 id，GUI 里能点回去 |
| `status` | `draft` → `active` → `retired`；被拒的直接是 `retired`，并附原因 |
| `author` / `approved_by` | 谁提的、谁批的 |
| `created_at` / `updated_at` | |

存在 hub 的 store 里（新表 `lessons`），和 `team` 笔记一样，每次写入都记事件，历史可查。

### 4.2 怎么记：`lesson_propose`

新协作工具，所有 agent 都能用：

```json
{ "text": "...", "why": "...", "scope": "repo" | "global", "replaces": "<id>?" }
```

- 结果是一条 `draft`，返回同范围里现有的 `active` 教训列表，让 agent 自己判断是不是重复。重复时应改为传 `replaces`，表示「这条更准，替换那条」。hub 不做相似度判断。
- 什么时候该记，写在工具说明和注入的教训小节末尾的一句话里，例如：「做到一半发现一件下一个 agent 也会踩的事（环境、工具、仓库的坑），用 `lesson_propose` 记一句。」
  - hub 不在任务失败、检查不过时自动发消息催 agent 记（[H1.md](H1.md) §2「转达，不代言」）。
  - 主导在 `run_finish` 的报告里本来就会写遇到的问题；`run_finish` 的工具说明加一句「值得留给以后的，另用 `lesson_propose`」。
- 也可以由人在 GUI 里直接写一条，写完就是 `active`。
- 复盘（SELF C1）的产出可以逐条用 `lesson_propose` 提上来。

### 4.3 谁批

| 情况 | 谁批 |
|---|---|
| 缺省 | 人，在 GUI 的教训页或 run 页批准、改字后批准、拒绝（附原因） |
| run 的 `policy.lessons = "lead"` | 本 run 内提出的 `repo` 范围教训由主导用 `lesson_review` 批，人事后能看到、能撤 |
| `global` 范围 | 只能由人批，不受上面的 policy 影响 |

不做完全自动批准。教训会进入每个 agent 的开场说明，等于一个能影响所有会话的提示入口，必须有人或主导过目。主导批准的范围也只限本仓库。

涉及权限、审批、守卫的教训（例如「遇到 X 可以跳过审批」）不管 policy 怎么设，一律交给人批。v1 不做内容判断，靠人在批 `global` 和审 run 时把关。是否加关键词拦截，见 §7。

### 4.4 怎么送达

- **新会话**：`rules.rs` 在项目规则之后加一节 `# Lessons`。内容是这个仓库的 `active` 教训加上 `global` 的教训，新的在前，每条一行，带 id。整节上限 8 KB，超出时截断并注明「还有 N 条，用 `lesson_list` 看全」。
  - 这一节对所有 agent 都注入，包括会自己读规则文件的 Claude 和 Codex，因为没有哪家会自己读 hub 里的东西。
- **正在跑的会话**：一条教训变成 `active` 时，hub 用现有的通知摘要（`notify`，topic `lessons`），把这一行发给同仓库里有会话在跑的 agent。这是转达人或主导的决定，不是 hub 的提示。
- **按需查**：`lesson_list { scope?, query? }`，列出全部 `active` 教训，不受 8 KB 限制。

### 4.5 怎么防止越堆越乱

- 每个仓库最多 40 条 `active`，`global` 最多 20 条。满了以后，批准新的一条要先在 GUI 里退役一条，或者用 `replaces` 替换。
- 退役：人在 GUI 里退役，或者 agent 用 `lesson_retire { id, why }` 提议退役（走和批准一样的流程）。典型场景是「这条已经过时：python 占位问题在新机器上不存在」。
- 不自动按时间过期：一条教训有没有用，和它是多久以前写的无关。

### 4.6 接口与事件

- REST（写级）：
  - `GET /v1/lessons?scope=&repo=&status=`
  - `POST /v1/lessons`（人直接写）
  - `POST /v1/lessons/{id}/approve`（可带改过的 `text`）
  - `POST /v1/lessons/{id}/reject { reason }`
  - `POST /v1/lessons/{id}/retire { reason }`
- 协作工具：`lesson_propose`、`lesson_list`、`lesson_retire`（提议），以及只对主导开放的 `lesson_review`（policy 允许时）。
- 事件：`lesson.proposed`、`lesson.approved`、`lesson.retired`。GUI 用它们刷新「待审教训」的计数。
- CLI：`crossagents lessons ls | approve | reject | retire | add`。

### 4.7 GUI

- **侧栏或设置页的「教训」页**：上半是待审的草稿，带出处链接、why，以及批准、改字、拒绝三个按钮；下半是生效中的列表，按仓库和 global 分组，每条可以退役。
- **run 页**：本 run 里提出的教训显示在收尾报告旁边，可以直接批。
- 有待审草稿时，「需要你」条里多一项。

## 5. 分阶段

| 阶段 | 内容 | 验收 |
|---|---|---|
| L1 | 表、REST、CLI；`lesson_propose` / `lesson_list`；人批；新会话注入 `# Lessons` | 单测：草稿不注入，批准后新会话的开场说明里有它，超过 8 KB 时截断并注明；mock-agent 集成测试走一遍提出、批准、注入 |
| L2 | 正在跑的会话收到新教训；GUI 教训页、run 页和「需要你」入口；数量上限与 `replaces` | 单测：批准后同仓库在跑的 agent 收到通知摘要，别的仓库收不到；mock 场景截图 |
| L3 | `policy.lessons = "lead"`；`lesson_review`；`lesson_retire` 提议；复盘（SELF C1）接入 | 单测：主导只能批本 run 的 `repo` 教训，批不了 `global` |

L1 做完就能用：教训全靠 agent 自觉提、人去批。L2、L3 解决的是送达和省人力。

## 6. 不做

- 不让 agent 改 `CROSSAGENTS.md` 或任何规则文件（§3）。
- 不在 hub 里写「请记一条教训」的催促消息（H1 §2）。
- 不做自动批准，也不做语义去重（交给 agent 看列表、人审）。
- 不把教训写进仓库文件。将来如果要随仓库流转（换机器、给别人用），再加 `crossagents lessons export` 导出成 `.crossagents/lessons.md` 由人提交，hub 仍以自己的表为准。

## 7. 待定

- 跨机器：多节点（[MULTI-NODE.md](../MULTI-NODE.md)）之后教训存在中心 hub，`global` 是否要区分到节点（「这台机器的 python 是占位」只对一台机器成立）。可能需要第三种范围 `node`。
- 是否对正文做最基本的拦截（出现 `skip approval`、`--dangerously` 之类时强制交给人批）。先看 L1 跑一阵后实际提上来的是什么。
- `team` 笔记里已有的「坑」要不要迁移成教训草稿，让人批一遍。
